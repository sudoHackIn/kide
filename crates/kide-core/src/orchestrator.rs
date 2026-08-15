//! Batch orchestration for cold language workers.

use std::{collections::BTreeSet, path::Path};

use prost::Message;
use thiserror::Error;

use crate::{
    AnalysisFact, AnalyzeBatchRequest, ArtifactAnalysisRequest, ArtifactBlobCache,
    ArtifactBlobCacheError, ArtifactBlobKey, ArtifactDescriptor, ArtifactDiscoveryRequest,
    ArtifactMaterializationRequest, FileAnalysisSnapshot, Fingerprint, IndexAction, IndexStore,
    IndexStoreError, ProjectManifest, SourceOrigin, SourceUnit, WorkerEnvelope, WorkerLaunch,
    WorkerMessage, WorkerSupervisor, WorkerSupervisorError, WorkspacePath, plan_invalidation,
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
    #[error("artifact blob cache failed: {0}")]
    BlobCache(#[from] ArtifactBlobCacheError),
    #[error("worker returned invalid staged artifact metadata")]
    InvalidStagedArtifact,
    #[error("worker returned an analysis response with an unexpected source unit {source_unit}")]
    UnexpectedSnapshot { source_unit: String },
    #[error("worker did not return a snapshot for requested source unit {source_unit}")]
    MissingSnapshot { source_unit: String },
    #[error("worker returned {received:?}, expected analysis_batch_response")]
    InvalidResponse { received: Box<WorkerMessage> },
}

/// Materializes a cache-miss artifact through the worker and atomically
/// promotes its staged bytes after Core verifies response metadata. The caller
/// owns `staging_directory` lifecycle and may share it with the worker process.
pub fn materialize_artifact(
    supervisor: &mut WorkerSupervisor,
    cache: &ArtifactBlobCache,
    workspace_root: WorkspacePath,
    artifact: ArtifactDescriptor,
    staging_directory: &Path,
    request_id: impl Into<String>,
) -> Result<bool, IndexOrchestratorError> {
    let request_id = request_id.into();
    let key = ArtifactBlobKey::new(artifact.source_unit.content.clone(), &artifact.provenance);
    if cache.open_blob(&key)?.is_some() {
        return Ok(false);
    }
    let response = supervisor.request(WorkerEnvelope::new(
        request_id,
        WorkerMessage::ArtifactMaterializationRequest(Box::new(ArtifactMaterializationRequest {
            workspace_root,
            artifact,
            staging_directory: staging_directory.to_string_lossy().into_owned(),
            blob_format_version: crate::artifact_blob_layout::ARTIFACT_BLOB_LAYOUT_VERSION,
        })),
    ))?;
    let WorkerMessage::ArtifactMaterializationResponse(response) = response.message else {
        return Err(IndexOrchestratorError::InvalidResponse {
            received: Box::new(response.message),
        });
    };
    let response = *response;
    let digest =
        sha256_bytes(&response.sha256).ok_or(IndexOrchestratorError::InvalidStagedArtifact)?;
    let staged = staging_directory.join(&response.staged_filename);
    if staged.file_name().and_then(|name| name.to_str()) != Some(response.staged_filename.as_str())
    {
        return Err(IndexOrchestratorError::InvalidStagedArtifact);
    }
    let promoted = cache
        .promote_staged(&key, &staged, response.byte_length, digest)
        .map_err(IndexOrchestratorError::from)?;
    // A promoted blob is immutable in the cache. The worker-created file is
    // only a verified hand-off buffer and must not retain a second full copy.
    std::fs::remove_file(staged).map_err(ArtifactBlobCacheError::from)?;
    Ok(promoted)
}

fn sha256_bytes(value: &Fingerprint) -> Option<[u8; 32]> {
    let hex = value.as_str().strip_prefix("sha256:")?;
    if hex.len() != 64 {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(digest)
}

/// Applies an incremental plan and only commits snapshots after the worker has
/// returned a complete batch whose ownership matches the request.
pub fn index_batch(
    store: &mut IndexStore,
    manifest: &ProjectManifest,
    current: &[SourceUnit],
    launch: WorkerLaunch,
) -> Result<IndexRun, IndexOrchestratorError> {
    index_batch_with_optional_cache(store, manifest, current, launch, None)
}

/// Cache-aware dependency indexing. A hit loads stored graph sections without
/// asking the worker to extract the JAR again; a miss materializes one staged
/// blob and promotes it before facts reach the project index.
pub fn index_batch_with_artifact_cache(
    store: &mut IndexStore,
    manifest: &ProjectManifest,
    current: &[SourceUnit],
    launch: WorkerLaunch,
    cache: &ArtifactBlobCache,
    staging_directory: &Path,
) -> Result<IndexRun, IndexOrchestratorError> {
    index_batch_with_optional_cache(
        store,
        manifest,
        current,
        launch,
        Some((cache, staging_directory)),
    )
}

fn index_batch_with_optional_cache(
    store: &mut IndexStore,
    manifest: &ProjectManifest,
    current: &[SourceUnit],
    launch: WorkerLaunch,
    artifact_cache: Option<(&ArtifactBlobCache, &Path)>,
) -> Result<IndexRun, IndexOrchestratorError> {
    let persisted_sources = store
        .source_units()?
        .into_iter()
        .filter(|source| source.origin != SourceOrigin::Dependency)
        .collect::<Vec<_>>();
    let actions = plan_invalidation(current, &persisted_sources);
    let _span = tracing::info_span!(target: "kide::index", "index_batch", sources = current.len())
        .entered();
    tracing::debug!(target: "kide::index", actions = actions.len(), "planned source actions");
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
    tracing::debug!(target: "kide::index", "starting worker handshake");
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
    tracing::debug!(target: "kide::index", "received source batch response");
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
    match artifact_cache {
        Some((_cache, _staging)) => index_dependency_catalog(store, &mut supervisor)?,
        None => index_dependencies(store, &mut supervisor, &mut run)?,
    }
    store.put_manifest(manifest)?;
    run.worker_starts = supervisor.start_count();
    Ok(run)
}

fn index_dependency_catalog(
    store: &IndexStore,
    supervisor: &mut WorkerSupervisor,
) -> Result<(), IndexOrchestratorError> {
    let mut cursor = None;
    let mut page = 0_u64;
    loop {
        let response = supervisor.request(WorkerEnvelope::new(
            format!("index-artifact-descriptors-{page}"),
            WorkerMessage::ArtifactDiscoveryRequest(ArtifactDiscoveryRequest {
                workspace_root: WorkspacePath::new("."),
                max_artifacts: 64,
                cursor: cursor.clone(),
            }),
        ))?;
        let WorkerMessage::ArtifactDiscoveryResponse(response) = response.message else {
            return Err(IndexOrchestratorError::InvalidResponse {
                received: Box::new(response.message),
            });
        };
        for descriptor in &response.artifacts {
            store.put_artifact_descriptor(descriptor)?;
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

#[allow(dead_code)] // Retained as the demand-materialization implementation while callers move to catalog-first.
fn index_dependencies_from_cache(
    store: &mut IndexStore,
    supervisor: &mut WorkerSupervisor,
    run: &mut IndexRun,
    cache: &ArtifactBlobCache,
    staging_directory: &Path,
) -> Result<(), IndexOrchestratorError> {
    let mut cursor = None;
    let mut page = 0_u64;
    loop {
        let response = supervisor.request(WorkerEnvelope::new(
            format!("index-artifact-descriptors-{page}"),
            WorkerMessage::ArtifactDiscoveryRequest(ArtifactDiscoveryRequest {
                workspace_root: WorkspacePath::new("."),
                max_artifacts: 1,
                cursor: cursor.clone(),
            }),
        ))?;
        let WorkerMessage::ArtifactDiscoveryResponse(response) = response.message else {
            return Err(IndexOrchestratorError::InvalidResponse {
                received: Box::new(response.message),
            });
        };
        for descriptor in response.artifacts {
            let key = ArtifactBlobKey::new(
                descriptor.source_unit.content.clone(),
                &descriptor.provenance,
            );
            let hit = cache.open_blob(&key)?.is_some();
            if !hit {
                materialize_artifact(
                    supervisor,
                    cache,
                    WorkspacePath::new("."),
                    descriptor.clone(),
                    staging_directory,
                    format!("index-artifact-materialize-{page}"),
                )?;
            }
            let payload = cache
                .load(&key)?
                .ok_or(IndexOrchestratorError::InvalidStagedArtifact)?;
            let layout = crate::artifact_blob_layout::ArtifactBlobLayout::validate(payload)
                .map_err(|_| IndexOrchestratorError::InvalidStagedArtifact)?;
            let graph = crate::artifact_proto::GraphArtifact::decode(
                layout
                    .section(crate::artifact_proto::ArtifactBlobSectionKind::GraphFacts)
                    .map_err(|_| IndexOrchestratorError::InvalidStagedArtifact)?,
            )
            .map_err(|_| IndexOrchestratorError::InvalidStagedArtifact)?;
            let snapshots = crate::artifact_proto_adapter::decode_graph_artifact(graph)
                .map_err(|_| IndexOrchestratorError::InvalidStagedArtifact)?;
            for snapshot in snapshots {
                let unchanged =
                    store
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
