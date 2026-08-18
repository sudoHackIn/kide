//! Planning and bounded execution of negotiated worker query capabilities.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use prost::Message;
use thiserror::Error;

use crate::{
    DiscoveredWorker, SemanticQueryArgument, SemanticQueryArgumentValue, SemanticQueryBudget,
    SemanticQueryCapability, SemanticQueryParameterType, SemanticQueryRequest,
    SemanticQueryResponse, SemanticQueryResponseState, SemanticQueryResultKind, WorkerEnvelope,
    WorkerIdentity, WorkerMessage, WorkerSupervisor, WorkerSupervisorError, worker_proto_adapter,
};

pub const MAX_CAPABILITY_CANDIDATES: u32 = 1_000;
pub const MAX_CAPABILITY_NODES: u32 = 10_000;
pub const MAX_CAPABILITY_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_CAPABILITY_DEADLINE_MILLIS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticCapabilityPlan {
    pub worker: WorkerIdentity,
    pub capability: SemanticQueryCapability,
    pub request: SemanticQueryRequest,
}

#[derive(Debug, Error)]
pub enum SemanticCapabilityError {
    #[error("worker does not advertise semantic capability `{name}` version {version}")]
    Unavailable { name: String, version: u32 },
    #[error("semantic capability arguments do not match the advertised contract: {0}")]
    InvalidArguments(String),
    #[error("semantic capability budget is invalid: {0}")]
    InvalidBudget(&'static str),
    #[error("worker supervisor failed: {0}")]
    Supervisor(#[from] WorkerSupervisorError),
    #[error("semantic capability protobuf validation failed: {0}")]
    Adapter(#[from] worker_proto_adapter::AdapterError),
    #[error("worker returned an unexpected semantic capability response: {0}")]
    InvalidResponse(String),
    #[error("semantic capability response exceeded the {0} budget")]
    BudgetExceeded(&'static str),
}

pub fn plan_semantic_capability(
    worker: &DiscoveredWorker,
    capability_name: &str,
    capability_version: u32,
    arguments: Vec<SemanticQueryArgument>,
    mut candidate_symbols: Vec<crate::SymbolId>,
    mut candidate_source_units: Vec<crate::SourceUnitId>,
    budget: SemanticQueryBudget,
) -> Result<SemanticCapabilityPlan, SemanticCapabilityError> {
    validate_budget(budget)?;
    if candidate_symbols.len() > budget.max_candidates as usize {
        return Err(SemanticCapabilityError::BudgetExceeded("candidate input"));
    }
    if candidate_source_units.len() > budget.max_nodes as usize {
        return Err(SemanticCapabilityError::BudgetExceeded("source input"));
    }
    let capability = worker
        .capabilities
        .semantic_query_capabilities
        .iter()
        .find(|capability| {
            capability.name == capability_name && capability.version == capability_version
        })
        .cloned()
        .ok_or_else(|| SemanticCapabilityError::Unavailable {
            name: capability_name.to_owned(),
            version: capability_version,
        })?;
    let arguments = validate_arguments(&capability, arguments)?;
    candidate_symbols.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    candidate_symbols.dedup_by(|left, right| left.as_str() == right.as_str());
    candidate_source_units.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    candidate_source_units.dedup_by(|left, right| left.as_str() == right.as_str());
    Ok(SemanticCapabilityPlan {
        worker: worker.capabilities.identity.clone(),
        capability,
        request: SemanticQueryRequest {
            capability_name: capability_name.to_owned(),
            capability_version,
            arguments,
            candidate_symbols,
            candidate_source_units,
            budget,
        },
    })
}

pub fn execute_semantic_capability(
    supervisor: &mut WorkerSupervisor,
    plan: &SemanticCapabilityPlan,
    request_id: impl Into<String>,
) -> Result<SemanticQueryResponse, SemanticCapabilityError> {
    let request_id = request_id.into();
    let handshake = supervisor.handshake(format!("{request_id}-handshake"))?;
    if handshake.capabilities.identity != plan.worker
        || !handshake
            .capabilities
            .semantic_query_capabilities
            .contains(&plan.capability)
    {
        return Err(SemanticCapabilityError::Unavailable {
            name: plan.capability.name.clone(),
            version: plan.capability.version,
        });
    }
    let started = Instant::now();
    let envelope = supervisor.request_with_timeout(
        WorkerEnvelope::new(
            request_id,
            WorkerMessage::SemanticQueryRequest(Box::new(plan.request.clone())),
        ),
        Duration::from_millis(plan.request.budget.deadline_millis),
    )?;
    let WorkerMessage::SemanticQueryResponse(response) = envelope.message else {
        return Err(SemanticCapabilityError::InvalidResponse(format!(
            "expected semantic_query_response, received {:?}",
            envelope.message
        )));
    };
    validate_semantic_capability_response(plan, *response, started.elapsed().as_millis() as u64)
}

pub fn validate_semantic_capability_response(
    plan: &SemanticCapabilityPlan,
    mut response: SemanticQueryResponse,
    elapsed_millis: u64,
) -> Result<SemanticQueryResponse, SemanticCapabilityError> {
    if response.capability_name != plan.request.capability_name
        || response.capability_version != plan.request.capability_version
    {
        return Err(SemanticCapabilityError::InvalidResponse(
            "capability identity does not match the request".to_owned(),
        ));
    }
    if response.provenance.backend != plan.worker.backend
        || response.provenance.backend_version != plan.worker.backend_version
        || response.provenance.protocol_version != crate::WORKER_PROTOCOL_VERSION
    {
        return Err(SemanticCapabilityError::InvalidResponse(
            "provenance does not match the negotiated worker".to_owned(),
        ));
    }
    if elapsed_millis > plan.request.budget.deadline_millis {
        return Err(SemanticCapabilityError::BudgetExceeded("deadline"));
    }
    if response.visited_nodes > plan.request.budget.max_nodes {
        return Err(SemanticCapabilityError::BudgetExceeded("node"));
    }
    if response.produced_bytes > plan.request.budget.max_bytes {
        return Err(SemanticCapabilityError::BudgetExceeded("reported byte"));
    }
    if response.candidate_symbols.len() > plan.request.budget.max_candidates as usize {
        return Err(SemanticCapabilityError::BudgetExceeded("candidate output"));
    }
    if response.snapshots.len() > plan.request.budget.max_nodes as usize {
        return Err(SemanticCapabilityError::BudgetExceeded("snapshot output"));
    }
    let encoded_bytes = worker_proto_adapter::semantic_query_response(&response)?.encoded_len();
    if encoded_bytes as u64 > plan.request.budget.max_bytes {
        return Err(SemanticCapabilityError::BudgetExceeded("encoded byte"));
    }
    match plan.capability.result {
        SemanticQueryResultKind::CandidateSymbols if !response.snapshots.is_empty() => {
            return Err(SemanticCapabilityError::InvalidResponse(
                "candidate-symbol capability returned normalized facts".to_owned(),
            ));
        }
        SemanticQueryResultKind::NormalizedFacts if !response.candidate_symbols.is_empty() => {
            return Err(SemanticCapabilityError::InvalidResponse(
                "normalized-facts capability returned candidate symbols".to_owned(),
            ));
        }
        _ => {}
    }
    if response.state == SemanticQueryResponseState::Unsupported
        && (!response.candidate_symbols.is_empty() || !response.snapshots.is_empty())
    {
        return Err(SemanticCapabilityError::InvalidResponse(
            "unsupported response contains results".to_owned(),
        ));
    }
    if response.state == SemanticQueryResponseState::Complete
        && response
            .snapshots
            .iter()
            .any(|snapshot| snapshot.completeness != crate::Completeness::Complete)
    {
        response.state = SemanticQueryResponseState::Partial;
    }
    let allowed_sources = plan
        .request
        .candidate_source_units
        .iter()
        .map(|source| source.as_str())
        .collect::<BTreeSet<_>>();
    if response
        .snapshots
        .iter()
        .any(|snapshot| !allowed_sources.contains(snapshot.source_unit.id.as_str()))
    {
        return Err(SemanticCapabilityError::InvalidResponse(
            "response contains a snapshot outside the bounded source set".to_owned(),
        ));
    }
    if response.snapshots.iter().any(|snapshot| {
        snapshot.provenance.backend != plan.worker.backend
            || snapshot.provenance.backend_version != plan.worker.backend_version
            || snapshot.provenance.protocol_version != crate::WORKER_PROTOCOL_VERSION
    }) {
        return Err(SemanticCapabilityError::InvalidResponse(
            "snapshot provenance does not match the negotiated worker".to_owned(),
        ));
    }
    response
        .candidate_symbols
        .sort_by(|left, right| left.as_str().cmp(right.as_str()));
    response
        .candidate_symbols
        .dedup_by(|left, right| left.as_str() == right.as_str());
    response.snapshots.sort_by(|left, right| {
        left.source_unit
            .id
            .as_str()
            .cmp(right.source_unit.id.as_str())
    });
    if response
        .snapshots
        .windows(2)
        .any(|pair| pair[0].source_unit.id.as_str() == pair[1].source_unit.id.as_str())
    {
        return Err(SemanticCapabilityError::InvalidResponse(
            "response contains duplicate source snapshots".to_owned(),
        ));
    }
    Ok(response)
}

fn validate_budget(budget: SemanticQueryBudget) -> Result<(), SemanticCapabilityError> {
    if budget.max_candidates == 0 || budget.max_candidates > MAX_CAPABILITY_CANDIDATES {
        return Err(SemanticCapabilityError::InvalidBudget("max_candidates"));
    }
    if budget.max_nodes == 0 || budget.max_nodes > MAX_CAPABILITY_NODES {
        return Err(SemanticCapabilityError::InvalidBudget("max_nodes"));
    }
    if budget.max_bytes == 0 || budget.max_bytes > MAX_CAPABILITY_BYTES {
        return Err(SemanticCapabilityError::InvalidBudget("max_bytes"));
    }
    if budget.deadline_millis == 0 || budget.deadline_millis > MAX_CAPABILITY_DEADLINE_MILLIS {
        return Err(SemanticCapabilityError::InvalidBudget("deadline_millis"));
    }
    Ok(())
}

fn validate_arguments(
    capability: &SemanticQueryCapability,
    arguments: Vec<SemanticQueryArgument>,
) -> Result<Vec<SemanticQueryArgument>, SemanticCapabilityError> {
    let declared = capability
        .parameters
        .iter()
        .map(|parameter| (parameter.name.as_str(), parameter))
        .collect::<BTreeMap<_, _>>();
    if declared.len() != capability.parameters.len() {
        return Err(SemanticCapabilityError::InvalidArguments(
            "advertised contract contains duplicate parameter names".to_owned(),
        ));
    }
    let mut supplied = BTreeMap::new();
    for argument in arguments {
        let Some(parameter) = declared.get(argument.name.as_str()) else {
            return Err(SemanticCapabilityError::InvalidArguments(format!(
                "unknown argument `{}`",
                argument.name
            )));
        };
        if supplied.contains_key(&argument.name) {
            return Err(SemanticCapabilityError::InvalidArguments(format!(
                "argument `{}` is supplied more than once",
                argument.name
            )));
        }
        if !argument_matches(parameter.ty, &argument.value) {
            return Err(SemanticCapabilityError::InvalidArguments(format!(
                "argument `{}` has the wrong type",
                argument.name
            )));
        }
        supplied.insert(argument.name.clone(), argument);
    }
    for parameter in &capability.parameters {
        if parameter.required && !supplied.contains_key(&parameter.name) {
            return Err(SemanticCapabilityError::InvalidArguments(format!(
                "required argument `{}` is missing",
                parameter.name
            )));
        }
    }
    Ok(supplied.into_values().collect())
}

fn argument_matches(
    expected: SemanticQueryParameterType,
    value: &SemanticQueryArgumentValue,
) -> bool {
    matches!(
        (expected, value),
        (
            SemanticQueryParameterType::SymbolId,
            SemanticQueryArgumentValue::SymbolId(_)
        ) | (
            SemanticQueryParameterType::ComponentId,
            SemanticQueryArgumentValue::ComponentId(_)
        ) | (
            SemanticQueryParameterType::String,
            SemanticQueryArgumentValue::String(_)
        ) | (
            SemanticQueryParameterType::Integer,
            SemanticQueryArgumentValue::Integer(_)
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Completeness, ComponentId, Fingerprint, Language, Provenance, SemanticQueryParameter,
        SourceOrigin, SourceUnit, SourceUnitId, SymbolId, WorkspacePath,
    };

    #[test]
    fn typed_arguments_and_core_budget_caps_are_enforced_before_launch() {
        let capability = SemanticQueryCapability {
            name: "types.assignable".to_owned(),
            version: 1,
            parameters: vec![SemanticQueryParameter {
                name: "target".to_owned(),
                ty: SemanticQueryParameterType::SymbolId,
                required: true,
            }],
            result: SemanticQueryResultKind::CandidateSymbols,
        };
        assert!(matches!(
            validate_arguments(
                &capability,
                vec![SemanticQueryArgument {
                    name: "target".to_owned(),
                    value: SemanticQueryArgumentValue::String("not-an-id".to_owned()),
                }]
            ),
            Err(SemanticCapabilityError::InvalidArguments(_))
        ));
        assert_eq!(
            validate_arguments(
                &capability,
                vec![SemanticQueryArgument {
                    name: "target".to_owned(),
                    value: SemanticQueryArgumentValue::SymbolId(SymbolId::new("target")),
                }]
            )
            .expect("typed argument")
            .len(),
            1
        );
        assert!(matches!(
            validate_budget(SemanticQueryBudget {
                max_candidates: MAX_CAPABILITY_CANDIDATES + 1,
                max_nodes: 1,
                max_bytes: 1,
                deadline_millis: 1,
            }),
            Err(SemanticCapabilityError::InvalidBudget("max_candidates"))
        ));
    }

    #[test]
    fn normalized_facts_stay_inside_source_bounds_and_cannot_overclaim_completeness() {
        let capability = SemanticQueryCapability {
            name: "flow.cfg".to_owned(),
            version: 1,
            parameters: Vec::new(),
            result: SemanticQueryResultKind::NormalizedFacts,
        };
        let identity = WorkerIdentity {
            backend: "fixture".to_owned(),
            backend_version: "1.0.0".to_owned(),
        };
        let source = SourceUnitId::new("fixture:allowed.kt");
        let plan = SemanticCapabilityPlan {
            worker: identity.clone(),
            capability,
            request: SemanticQueryRequest {
                capability_name: "flow.cfg".to_owned(),
                capability_version: 1,
                arguments: Vec::new(),
                candidate_symbols: Vec::new(),
                candidate_source_units: vec![source.clone()],
                budget: SemanticQueryBudget {
                    max_candidates: 1,
                    max_nodes: 10,
                    max_bytes: 4096,
                    deadline_millis: 1000,
                },
            },
        };
        let provenance = Provenance {
            backend: identity.backend,
            backend_version: identity.backend_version,
            protocol_version: crate::WORKER_PROTOCOL_VERSION,
            analysis_options: Fingerprint::new("sha256:flow"),
        };
        let response = SemanticQueryResponse {
            capability_name: "flow.cfg".to_owned(),
            capability_version: 1,
            state: SemanticQueryResponseState::Complete,
            candidate_symbols: Vec::new(),
            snapshots: vec![empty_snapshot(source, provenance.clone())],
            provenance: provenance.clone(),
            visited_nodes: 1,
            produced_bytes: 1,
        };
        assert_eq!(
            validate_semantic_capability_response(&plan, response, 1)
                .expect("bounded partial facts")
                .state,
            SemanticQueryResponseState::Partial
        );
        let outside = SemanticQueryResponse {
            capability_name: "flow.cfg".to_owned(),
            capability_version: 1,
            state: SemanticQueryResponseState::Complete,
            candidate_symbols: Vec::new(),
            snapshots: vec![empty_snapshot(
                SourceUnitId::new("fixture:outside.kt"),
                provenance.clone(),
            )],
            provenance,
            visited_nodes: 1,
            produced_bytes: 1,
        };
        assert!(matches!(
            validate_semantic_capability_response(&plan, outside, 1),
            Err(SemanticCapabilityError::InvalidResponse(_))
        ));
    }

    fn empty_snapshot(id: SourceUnitId, provenance: Provenance) -> crate::FileAnalysisSnapshot {
        crate::FileAnalysisSnapshot {
            source_unit: SourceUnit {
                id,
                component: ComponentId::new("fixture:main"),
                path: WorkspacePath::new("fixture.kt"),
                language: Language::Kotlin,
                origin: SourceOrigin::Source,
                content: Fingerprint::new("sha256:content"),
                context: Fingerprint::new("sha256:context"),
            },
            structural_fingerprint: None,
            public_api_fingerprint: None,
            symbols: Vec::new(),
            applications: Vec::new(),
            occurrences: Vec::new(),
            references: Vec::new(),
            calls: Vec::new(),
            hierarchy: Vec::new(),
            types: Vec::new(),
            diagnostics: Vec::new(),
            completeness: Completeness::Partial,
            provenance,
        }
    }
}
