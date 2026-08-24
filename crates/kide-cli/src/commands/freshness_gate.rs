//! Owner-scoped freshness checks shared by semantic command frontends.

use std::collections::BTreeSet;

use anyhow::Result;
use kide_core::{IndexStore, QueryPayload, QueryProblem, document_from_bytes};

use super::WorkspaceContext;

pub(super) fn freshness_problem(
    context: &WorkspaceContext,
    store: &IndexStore,
    payload: &QueryPayload,
) -> Result<Option<QueryProblem>> {
    if context.configuration.freshness_strategy != kide_core::FreshnessStrategy::FreshOnly {
        return Ok(None);
    }
    let persisted_inputs = store.configuration_inputs()?;
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
    let owners = match payload {
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
    for owner in owners
        .into_iter()
        .map(|owner| owner.as_str().to_owned())
        .collect::<BTreeSet<_>>()
    {
        let Some(source) = store.source_unit(&kide_core::SourceUnitId::new(owner))? else {
            continue;
        };
        if source.origin == kide_core::SourceOrigin::Dependency {
            continue;
        }
        let current = std::fs::read(context.path().join(source.path.as_str()))
            .ok()
            .and_then(|bytes| document_from_bytes(source.path.clone(), bytes).ok());
        if !matches!(current, Some(current) if current.fingerprint == source.content) {
            return Ok(Some(QueryProblem {
                code: "stale_source_snapshot".to_owned(),
                message: "a source owning this result changed after indexing; run kide index"
                    .to_owned(),
                retryable: true,
            }));
        }
    }
    Ok(None)
}
