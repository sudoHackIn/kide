//! Batch orchestration for cold language workers.

use std::collections::BTreeSet;

use thiserror::Error;

use crate::{
    AnalysisFact, AnalyzeBatchRequest, ArtifactAnalysisRequest, FileAnalysisSnapshot, IndexAction,
    IndexStore, IndexStoreError, ProjectManifest, SourceOrigin, SourceUnit, WorkerEnvelope,
    WorkerLaunch, WorkerMessage, WorkerSupervisor, WorkerSupervisorError, WorkspacePath,
    plan_invalidation,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRun {
    pub reused: usize,
    pub analyzed: usize,
    pub removed: usize,
    pub worker_starts: u64,
    pub dependency_analyzed: usize,
    pub dependency_reused: usize,
}

#[derive(Debug, Error)]
pub enum IndexOrchestratorError {
    #[error("worker supervisor failed: {0}")]
    Worker(#[from] WorkerSupervisorError),
    #[error("index store failed: {0}")]
    Store(#[from] IndexStoreError),
    #[error("worker returned an analysis response with an unexpected source unit {source_unit}")]
    UnexpectedSnapshot { source_unit: String },
    #[error("worker did not return a snapshot for requested source unit {source_unit}")]
    MissingSnapshot { source_unit: String },
    #[error("worker returned {received:?}, expected analysis_batch_response")]
    InvalidResponse { received: Box<WorkerMessage> },
}

/// Applies an incremental plan and only commits snapshots after the worker has
/// returned a complete batch whose ownership matches the request.
pub fn index_batch(
    store: &mut IndexStore,
    manifest: &ProjectManifest,
    current: &[SourceUnit],
    launch: WorkerLaunch,
) -> Result<IndexRun, IndexOrchestratorError> {
    let persisted_sources = store
        .source_units()?
        .into_iter()
        .filter(|source| source.origin != SourceOrigin::Dependency)
        .collect::<Vec<_>>();
    let actions = plan_invalidation(current, &persisted_sources);
    let mut reanalyze = Vec::new();
    let mut run = IndexRun {
        reused: 0,
        analyzed: 0,
        removed: 0,
        worker_starts: 0,
        dependency_analyzed: 0,
        dependency_reused: 0,
    };
    for action in actions {
        match action {
            IndexAction::Reuse(_) => run.reused += 1,
            IndexAction::Reanalyze { source_unit, .. } => reanalyze.push(source_unit),
            IndexAction::Remove { source_unit } => {
                store.remove_snapshot(&source_unit)?;
                run.removed += 1;
            }
        }
    }
    // An unchanged manifest/source context also means the artifact model has
    // not changed, so do not start a worker merely to rediscover dependencies.
    if reanalyze.is_empty() {
        store.put_manifest(manifest)?;
        return Ok(run);
    }
    let mut supervisor = WorkerSupervisor::new(launch);
    supervisor.handshake("index-handshake")?;
    let response = supervisor.request(WorkerEnvelope::new(
        "index-batch",
        WorkerMessage::AnalyzeBatchRequest(AnalyzeBatchRequest {
            workspace: manifest.workspace.clone(),
            project_fingerprint: manifest.fingerprint.clone(),
            requested_facts: vec![
                AnalysisFact::Symbols,
                AnalysisFact::Occurrences,
                AnalysisFact::References,
                AnalysisFact::Calls,
                AnalysisFact::Hierarchy,
                AnalysisFact::Types,
            ],
            source_units: reanalyze.clone(),
        }),
    ))?;
    let WorkerMessage::AnalysisBatchResponse(response) = response.message else {
        return Err(IndexOrchestratorError::InvalidResponse {
            received: Box::new(response.message),
        });
    };
    let snapshots = validate_batch(&reanalyze, response.snapshots)?;
    for expected in &reanalyze {
        let snapshot = snapshots
            .iter()
            .find(|snapshot| snapshot.source_unit.id == expected.id)
            .expect("validated batch contains every requested unit");
        store.replace_snapshot(expected, snapshot)?;
        run.analyzed += 1;
    }
    index_dependencies(store, &mut supervisor, &mut run)?;
    store.put_manifest(manifest)?;
    run.worker_starts = supervisor.start_count();
    Ok(run)
}

fn index_dependencies(
    store: &mut IndexStore,
    supervisor: &mut WorkerSupervisor,
    run: &mut IndexRun,
) -> Result<(), IndexOrchestratorError> {
    let mut cursor = None;
    let mut page = 0_u64;
    loop {
        let response = supervisor.request(WorkerEnvelope::new(
            format!("index-artifacts-{page}"),
            WorkerMessage::ArtifactAnalysisRequest(ArtifactAnalysisRequest {
                workspace_root: WorkspacePath::new("."),
                max_artifacts: 1,
                cursor: cursor.clone(),
            }),
        ))?;
        let WorkerMessage::ArtifactAnalysisResponse(response) = response.message else {
            return Err(IndexOrchestratorError::InvalidResponse {
                received: Box::new(response.message),
            });
        };
        for snapshot in response.snapshots {
            let unchanged = store
                .source_unit(&snapshot.source_unit.id)?
                .is_some_and(|previous| {
                    previous.content == snapshot.source_unit.content
                        && previous.context == snapshot.source_unit.context
                });
            if unchanged {
                run.dependency_reused += 1;
            } else {
                store.replace_snapshot(&snapshot.source_unit, &snapshot)?;
                run.dependency_analyzed += 1;
            }
        }
        match response.next_cursor {
            Some(next) => {
                cursor = Some(next);
                page += 1
            }
            None => return Ok(()),
        }
    }
}

fn validate_batch(
    requested: &[SourceUnit],
    snapshots: Vec<FileAnalysisSnapshot>,
) -> Result<Vec<FileAnalysisSnapshot>, IndexOrchestratorError> {
    let expected = requested
        .iter()
        .map(|unit| unit.id.as_str())
        .collect::<BTreeSet<_>>();
    let received = snapshots
        .iter()
        .map(|snapshot| snapshot.source_unit.id.as_str())
        .collect::<BTreeSet<_>>();
    if let Some(extra) = received.difference(&expected).next() {
        return Err(IndexOrchestratorError::UnexpectedSnapshot {
            source_unit: (*extra).to_owned(),
        });
    }
    if let Some(missing) = expected.difference(&received).next() {
        return Err(IndexOrchestratorError::MissingSnapshot {
            source_unit: (*missing).to_owned(),
        });
    }
    Ok(snapshots)
}
