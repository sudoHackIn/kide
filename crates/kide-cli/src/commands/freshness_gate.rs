//! Owner-scoped freshness checks shared by semantic command frontends.

use std::collections::BTreeSet;

use anyhow::Result;
use kide_core::{IndexStore, QueryPayload, QueryProblem, document_from_bytes};

use super::WorkspaceContext;

pub(super) fn stale_source_problem() -> QueryProblem {
    QueryProblem {
        code: "stale_source_snapshot".to_owned(),
        message: "a source owning this result changed after indexing; run kide index".to_owned(),
        retryable: true,
    }
}

pub(super) fn freshness_problem(
    context: &WorkspaceContext,
    store: &IndexStore,
    payload: &QueryPayload,
) -> Result<Option<QueryProblem>> {
    let _gate = tracing::debug_span!(
        target: "kide::freshness",
        "freshness_gate",
        payload = payload_kind(payload),
    )
    .entered();
    if context.configuration.freshness_strategy != kide_core::FreshnessStrategy::FreshOnly {
        return Ok(None);
    }
    let persisted_inputs = {
        let _span = tracing::debug_span!(
            target: "kide::freshness",
            "load_configuration_inputs",
        )
        .entered();
        store.configuration_inputs()?
    };
    let _configuration = tracing::debug_span!(
        target: "kide::freshness",
        "fingerprint_configuration_inputs",
        inputs = persisted_inputs.len(),
    )
    .entered();
    for input in &persisted_inputs {
        if input.path.as_str() == ".kide/effective-config" {
            continue;
        }
        let current = kide_core::fingerprint_file(&context.path().join(input.path.as_str())).ok();
        if current.as_ref() != Some(&input.fingerprint) {
            return Ok(Some(QueryProblem {
                code: "stale_configuration_input".to_owned(),
                message: "a build or workspace configuration input changed after indexing; run kide index"
                    .to_owned(),
                retryable: true,
            }));
        }
    }
    if let Some(persisted) = persisted_inputs
        .iter()
        .find(|input| input.path.as_str() == ".kide/effective-config")
    {
        let Some(manifest) = store.latest_manifest()? else {
            return Ok(Some(QueryProblem {
                code: "missing_workspace_manifest".to_owned(),
                message: "the index has no resolved workspace manifest; run kide index".to_owned(),
                retryable: true,
            }));
        };
        let current = super::index::effective_configuration_input(
            &context.configuration,
            &manifest.components,
        )?;
        if current.fingerprint != persisted.fingerprint {
            return Ok(Some(QueryProblem {
                code: "stale_effective_configuration".to_owned(),
                message: "the effective KIDE configuration changed after indexing; run kide index"
                    .to_owned(),
                retryable: true,
            }));
        }
    }
    drop(_configuration);
    let owner_ids = match payload {
        QueryPayload::Symbols { symbols } | QueryPayload::Implementations { symbols } => symbols
            .iter()
            .map(|symbol| symbol.declaration.source_unit.clone())
            .collect(),
        QueryPayload::Definition { symbol } => vec![symbol.declaration.source_unit.clone()],
        QueryPayload::Refs { references } => references
            .iter()
            .map(|reference| reference.range.source_unit.clone())
            .collect(),
        QueryPayload::Callers { calls } => calls
            .iter()
            .map(|call| call.range.source_unit.clone())
            .collect(),
        QueryPayload::TypeAt { occurrence, .. } => vec![occurrence.range.source_unit.clone()],
        _ => Vec::new(),
    };
    let owner_ids = owner_ids
        .into_iter()
        .map(|owner| owner.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    let owners = {
        let _span = tracing::debug_span!(
            target: "kide::freshness",
            "load_owner_snapshots",
            owners = owner_ids.len(),
        )
        .entered();
        owner_ids
            .into_iter()
            .map(|owner| store.source_unit(&kide_core::SourceUnitId::new(owner)))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
    };
    let _sources = tracing::debug_span!(
        target: "kide::freshness",
        "stat_or_hash_owners",
        owners = owners.len(),
    )
    .entered();
    for source in owners {
        if source.origin == kide_core::SourceOrigin::Dependency {
            continue;
        }
        let current = std::fs::read(context.path().join(source.path.as_str()))
            .ok()
            .and_then(|bytes| document_from_bytes(source.path.clone(), bytes).ok());
        if !matches!(current, Some(current) if current.fingerprint == source.content) {
            return Ok(Some(stale_source_problem()));
        }
    }
    Ok(None)
}

fn payload_kind(payload: &QueryPayload) -> &'static str {
    match payload {
        QueryPayload::Index(_) => "index",
        QueryPayload::Symbols { .. } => "symbols",
        QueryPayload::Definition { .. } => "definition",
        QueryPayload::Refs { .. } => "refs",
        QueryPayload::Callers { .. } => "callers",
        QueryPayload::Implementations { .. } => "implementations",
        QueryPayload::TypeAt { .. } => "type_at",
        QueryPayload::Status { .. } => "status",
        QueryPayload::Selector { .. } => "selector",
    }
}
