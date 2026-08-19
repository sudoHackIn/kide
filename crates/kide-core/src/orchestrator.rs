//! Batch orchestration for cold language workers.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Instant,
};

use thiserror::Error;

use crate::{
    plan_invalidation, AnalysisFact, AnalysisInput, AnalyzeBatchRequest, ArtifactBlobCache,
    ArtifactBlobCacheError, ArtifactBlobKey, ArtifactCandidate, ArtifactDescriptor, ArtifactDiscoveryRequest,
    ArtifactMaterializationRequest, FileAnalysisSnapshot, Fingerprint, IndexAction, IndexStore,
    IndexStoreError, ProjectManifest, Provenance, SourceOrigin, SourceUnit, WorkerBatch,
    WorkerCapability, WorkerEnvelope, WorkerLaunch, WorkerMessage, WorkerSelection,
    WorkerSupervisor, WorkerSupervisorError, WorkspacePath,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRun {
    pub reused: usize,
    pub analyzed: usize,
    pub removed: usize,
    pub worker_starts: u64,
    pub dependency_analyzed: usize,
    pub dependency_reused: usize,
    pub dependency_catalog_millis: u128,
    pub dependency_materialization_millis: u128,
    /// Wall time spent awaiting source-analysis worker responses.
    pub source_worker_millis: u128,
    /// Wall time committing validated source snapshots to SQLite.
    pub source_commit_millis: u128,
    pub worker_phase_millis: BTreeMap<String, u64>,
    pub worker_metrics: BTreeMap<String, u64>,
    pub batches: Vec<BatchIndexMetrics>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BatchIndexMetrics {
    pub component: String,
    pub language: String,
    pub source_units: usize,
    pub worker_millis: u128,
    pub commit_millis: u128,
    pub worker_phases: BTreeMap<String, u64>,
    pub worker_metrics: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaterializationBudget {
    pub remaining_artifacts: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializationOutcome {
    Materialized,
    AlreadyMaterialized,
    BudgetExhausted,
    NotCataloged,
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
    #[error("no compatible worker for {count} source batch(es)")]
    UnsupportedBatches { count: usize },
    #[error("worker {worker} no longer supports batch {component} ({language:?})")]
    WorkerNoLongerCompatible {
        worker: String,
        component: String,
        language: crate::Language,
    },
}

/// Applies one global invalidation plan, then runs each selected worker for
/// its component/language batch. Identical worker launches stay alive for the
/// duration of this index run, avoiding a JVM cold start for every source
/// shard. The global plan is important: a per-batch invalidation pass would
/// incorrectly delete another language's persisted source units.
pub fn index_selected_batches(
    store: &mut IndexStore,
    manifest: &ProjectManifest,
    current: &[SourceUnit],
    selection: &WorkerSelection,
) -> Result<IndexRun, IndexOrchestratorError> {
    if !selection.unsupported.is_empty() {
        return Err(IndexOrchestratorError::UnsupportedBatches {
            count: selection.unsupported.len(),
        });
    }
    let persisted_sources = store
        .source_units()?
        .into_iter()
        .filter(|source| source.origin != SourceOrigin::Dependency)
        .collect::<Vec<_>>();
    let actions = plan_invalidation(current, &persisted_sources);
    let mut reanalyze = BTreeSet::new();
    let mut run = IndexRun {
        reused: 0,
        analyzed: 0,
        removed: 0,
        worker_starts: 0,
        dependency_analyzed: 0,
        dependency_reused: 0,
        dependency_catalog_millis: 0,
        dependency_materialization_millis: 0,
        source_worker_millis: 0,
        source_commit_millis: 0,
        worker_phase_millis: BTreeMap::new(),
        worker_metrics: BTreeMap::new(),
        batches: Vec::new(),
    };
    for action in actions {
        match action {
            IndexAction::Reuse(_) => run.reused += 1,
            IndexAction::Reanalyze { source_unit, .. } => {
                reanalyze.insert(source_unit.id.as_str().to_owned());
            }
            IndexAction::Remove { source_unit } => {
                store.remove_snapshot(&source_unit)?;
                run.removed += 1;
            }
        }
    }
    // A worker is disposable at the boundary of an index run, but reuse it
    // within that run. This keeps memory bounded while making a cold index
    // with many source shards pay the JVM startup cost only once per launch.
    let mut supervisors = Vec::<(WorkerLaunch, WorkerSupervisor)>::new();
    let mut artifact_candidates = BTreeMap::new();
    for (index, batch) in selection.batches.iter().enumerate() {
        let requested = batch
            .source_units
            .iter()
            .filter(|source| reanalyze.contains(source.id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if requested.is_empty() {
            continue;
        }
        let supervisor = if let Some(position) = supervisors
            .iter()
            .position(|(launch, _)| launch == &batch.worker.installation.launch)
        {
            &mut supervisors[position].1
        } else {
            supervisors.push((
                batch.worker.installation.launch.clone(),
                WorkerSupervisor::new(batch.worker.installation.launch.clone()),
            ));
            &mut supervisors
                .last_mut()
                .expect("a supervisor was just added")
                .1
        };
        analyze_selected_batch(
            store, manifest, batch, requested, index, supervisor, &mut run, &mut artifact_candidates,
        )?;
    }
    if !reanalyze.is_empty() {
        let catalog_batch = selection
            .batches
            .first()
            .expect("a supported selection has at least one batch");
        let supervisor = if let Some(position) = supervisors
            .iter()
            .position(|(launch, _)| launch == &catalog_batch.worker.installation.launch)
        {
            &mut supervisors[position].1
        } else {
            supervisors.push((
                catalog_batch.worker.installation.launch.clone(),
                WorkerSupervisor::new(catalog_batch.worker.installation.launch.clone()),
            ));
            let supervisor = &mut supervisors
                .last_mut()
                .expect("a supervisor was just added")
                .1;
            supervisor.handshake("index-dependency-catalog")?;
            supervisor
        };
        let catalog_started = Instant::now();
        index_dependency_catalog(store, supervisor, artifact_candidates.into_values().collect())?;
        run.dependency_catalog_millis += catalog_started.elapsed().as_millis();
    }
    run.worker_starts = supervisors
        .iter()
        .map(|(_, supervisor)| supervisor.start_count())
        .sum();
    store.put_manifest(manifest)?;
    Ok(run)
}

fn analyze_selected_batch(
    store: &mut IndexStore,
    manifest: &ProjectManifest,
    batch: &WorkerBatch,
    requested: Vec<SourceUnit>,
    batch_index: usize,
    supervisor: &mut WorkerSupervisor,
    run: &mut IndexRun,
    artifact_candidates: &mut BTreeMap<String, ArtifactCandidate>,
) -> Result<(), IndexOrchestratorError> {
    // `is_running` also reaps an exited or idle process. A restarted worker
    // must handshake again before it can receive a batch.
    if !supervisor.is_running() {
        let handshake = supervisor.handshake(format!("index-handshake-{batch_index}"))?;
        let compatible = handshake.capabilities.languages.contains(&batch.language)
            && handshake
                .capabilities
                .capabilities
                .contains(&WorkerCapability::FileAnalysisSnapshot);
        if !compatible {
            return Err(IndexOrchestratorError::WorkerNoLongerCompatible {
                worker: batch.worker.installation.name.clone(),
                component: batch.component.as_str().to_owned(),
                language: batch.language.clone(),
            });
        }
    }
    let worker_started = Instant::now();
    let response = supervisor.request(WorkerEnvelope::new(
        format!("index-batch-{batch_index}"),
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
            source_units: requested.clone(),
        }),
    ))?;
    let worker_millis = worker_started.elapsed().as_millis();
    run.source_worker_millis += worker_millis;
    tracing::info!(
        target: "kide::index",
        batch_index,
        component = batch.component.as_str(),
        language = ?batch.language,
        source_units = requested.len(),
        worker_millis,
        "source batch analyzed"
    );
    let WorkerMessage::AnalysisBatchResponse(response) = response.message else {
        return Err(IndexOrchestratorError::InvalidResponse {
            received: Box::new(response.message),
        });
    };
    for candidate in &response.artifact_candidates {
        artifact_candidates.entry(candidate.locator.clone()).or_insert_with(|| candidate.clone());
    }
    let snapshots = validate_batch(&requested, response.snapshots)?;
    let mut worker_phases = BTreeMap::new();
    for timing in response.timings {
        *worker_phases.entry(timing.phase.clone()).or_default() += timing.elapsed_millis;
        *run.worker_phase_millis.entry(timing.phase).or_default() += timing.elapsed_millis;
    }
    let mut worker_metrics = BTreeMap::new();
    for metric in response.metrics {
        *worker_metrics.entry(metric.name.clone()).or_default() += metric.value;
        *run.worker_metrics.entry(metric.name).or_default() += metric.value;
    }
    let commit_started = Instant::now();
    let entries = snapshots
        .iter()
        .map(|snapshot| (&snapshot.source_unit, snapshot))
        .collect::<Vec<_>>();
    store.replace_snapshots_batch(&entries)?;
    run.analyzed += snapshots.len();
    run.source_commit_millis += commit_started.elapsed().as_millis();
    run.batches.push(BatchIndexMetrics {
        component: batch.component.as_str().to_owned(),
        language: format!("{:?}", batch.language).to_lowercase(),
        source_units: requested.len(),
        worker_millis,
        commit_millis: commit_started.elapsed().as_millis(),
        worker_phases,
        worker_metrics,
    });
    tracing::debug!(
        target: "kide::index",
        batch_index,
        source_units = requested.len(),
        commit_millis = commit_started.elapsed().as_millis(),
        "source batch committed"
    );
    Ok(())
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

/// Resolves one catalog descriptor on demand. This is deliberately bounded:
/// callers receive `BudgetExhausted` rather than triggering a classpath scan.
pub fn materialize_catalog_artifact(
    store: &mut IndexStore,
    supervisor: &mut WorkerSupervisor,
    cache: &ArtifactBlobCache,
    workspace_root: WorkspacePath,
    source_unit: &crate::SourceUnitId,
    staging_directory: &Path,
    budget: &mut MaterializationBudget,
) -> Result<MaterializationOutcome, IndexOrchestratorError> {
    if store.source_unit(source_unit)?.is_some() {
        return Ok(MaterializationOutcome::AlreadyMaterialized);
    }
    let Some(descriptor) = store.artifact_descriptor(source_unit)? else {
        return Ok(MaterializationOutcome::NotCataloged);
    };
    if budget.remaining_artifacts == 0 {
        return Ok(MaterializationOutcome::BudgetExhausted);
    }
    budget.remaining_artifacts -= 1;
    let key = ArtifactBlobKey::new(
        descriptor.source_unit.content.clone(),
        &descriptor.provenance,
    );
    materialize_artifact(
        supervisor,
        cache,
        workspace_root,
        descriptor,
        staging_directory,
        format!("demand-materialize-{}", source_unit.as_str()),
    )?;
    let payload = cache
        .load(&key)?
        .ok_or(IndexOrchestratorError::InvalidStagedArtifact)?;
    let layout = crate::artifact_blob_layout::ArtifactBlobLayout::validate(payload)
        .map_err(|_| IndexOrchestratorError::InvalidStagedArtifact)?;
    let graph = layout
        .graph_facts()
        .map_err(|_| IndexOrchestratorError::InvalidStagedArtifact)?;
    for snapshot in crate::artifact_proto_adapter::decode_graph_artifact(graph)
        .map_err(|_| IndexOrchestratorError::InvalidStagedArtifact)?
    {
        store.replace_snapshot(&snapshot.source_unit, &snapshot)?;
    }
    Ok(MaterializationOutcome::Materialized)
}

/// Promotes exactly one cataloged artifact into the immutable blob cache. It
/// intentionally does not decode graph facts or write any project-index rows.
pub fn cache_catalog_artifact(
    store: &IndexStore,
    supervisor: &mut WorkerSupervisor,
    cache: &ArtifactBlobCache,
    workspace_root: WorkspacePath,
    source_unit: &crate::SourceUnitId,
    staging_directory: &Path,
    budget: &mut MaterializationBudget,
) -> Result<MaterializationOutcome, IndexOrchestratorError> {
    let Some(descriptor) = store.artifact_descriptor(source_unit)? else {
        return Ok(MaterializationOutcome::NotCataloged);
    };
    let key = ArtifactBlobKey::new(
        descriptor.source_unit.content.clone(),
        &descriptor.provenance,
    );
    if cache.open_blob(&key)?.is_some() {
        return Ok(MaterializationOutcome::AlreadyMaterialized);
    }
    if budget.remaining_artifacts == 0 {
        return Ok(MaterializationOutcome::BudgetExhausted);
    }
    budget.remaining_artifacts -= 1;
    if materialize_artifact(
        supervisor,
        cache,
        workspace_root,
        descriptor,
        staging_directory,
        format!("cache-materialize-{}", source_unit.as_str()),
    )? {
        Ok(MaterializationOutcome::Materialized)
    } else {
        Ok(MaterializationOutcome::AlreadyMaterialized)
    }
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
    index_batch_with_optional_cache(store, manifest, current, launch, None, None)
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
        None,
    )
}

pub fn index_batch_with_artifact_cache_and_provenance(
    store: &mut IndexStore,
    manifest: &ProjectManifest,
    current: &[SourceUnit],
    launch: WorkerLaunch,
    cache: &ArtifactBlobCache,
    staging_directory: &Path,
    provenance: Provenance,
) -> Result<IndexRun, IndexOrchestratorError> {
    index_batch_with_optional_cache(
        store,
        manifest,
        current,
        launch,
        Some((cache, staging_directory)),
        Some(provenance),
    )
}

fn index_batch_with_optional_cache(
    store: &mut IndexStore,
    manifest: &ProjectManifest,
    current: &[SourceUnit],
    launch: WorkerLaunch,
    _artifact_cache: Option<(&ArtifactBlobCache, &Path)>,
    current_provenance: Option<Provenance>,
) -> Result<IndexRun, IndexOrchestratorError> {
    let persisted_sources = store
        .source_units()?
        .into_iter()
        .filter(|source| source.origin != SourceOrigin::Dependency)
        .collect::<Vec<_>>();
    let actions = if let Some(provenance) = current_provenance {
        crate::plan_analysis_invalidation(
            &current
                .iter()
                .cloned()
                .map(|source_unit| AnalysisInput {
                    source_unit,
                    provenance: provenance.clone(),
                    public_api_fingerprint: None,
                })
                .collect::<Vec<_>>(),
            &store.analysis_inputs()?,
        )
    } else {
        plan_invalidation(current, &persisted_sources)
    };
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
        dependency_catalog_millis: 0,
        dependency_materialization_millis: 0,
        source_worker_millis: 0,
        source_commit_millis: 0,
        worker_phase_millis: BTreeMap::new(),
        worker_metrics: BTreeMap::new(),
        batches: Vec::new(),
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
    let worker_started = Instant::now();
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
    let worker_millis = worker_started.elapsed().as_millis();
    run.source_worker_millis += worker_millis;
    tracing::info!(
        target: "kide::index",
        source_units = reanalyze.len(),
        worker_millis,
        "source batch analyzed"
    );
    tracing::debug!(target: "kide::index", "received source batch response");
    let WorkerMessage::AnalysisBatchResponse(response) = response.message else {
        return Err(IndexOrchestratorError::InvalidResponse {
            received: Box::new(response.message),
        });
    };
    let snapshots = validate_batch(&reanalyze, response.snapshots)?;
    let mut worker_phases = BTreeMap::new();
    for timing in response.timings {
        *worker_phases.entry(timing.phase.clone()).or_default() += timing.elapsed_millis;
        *run.worker_phase_millis.entry(timing.phase).or_default() += timing.elapsed_millis;
    }
    let mut worker_metrics = BTreeMap::new();
    for metric in response.metrics {
        *worker_metrics.entry(metric.name.clone()).or_default() += metric.value;
        *run.worker_metrics.entry(metric.name).or_default() += metric.value;
    }
    // Keep the same cold worker alive for its dependency catalog request;
    // API-impact persistence below may perform SQLite work beyond its idle
    // timeout but requires no worker state.
    let catalog_started = Instant::now();
    index_dependency_catalog(store, &mut supervisor, response.artifact_candidates.clone())?;
    run.dependency_catalog_millis += catalog_started.elapsed().as_millis();
    let previous_inputs = store.analysis_inputs()?;
    let mut api_dependents = Vec::new();
    let mut entries = Vec::with_capacity(reanalyze.len());
    let commit_started = Instant::now();
    for expected in &reanalyze {
        let snapshot = snapshots
            .iter()
            .find(|snapshot| snapshot.source_unit.id == expected.id)
            .expect("validated batch contains every requested unit");
        let previous_api = previous_inputs
            .iter()
            .find(|input| input.source_unit.id == expected.id)
            .and_then(|input| input.public_api_fingerprint.as_ref());
        let dependents = store.dependent_source_units(&expected.id)?;
        entries.push((expected, snapshot));
        api_dependents.extend(crate::api_dependent_invalidations(
            previous_api,
            snapshot.public_api_fingerprint.as_ref(),
            dependents,
        ));
    }
    store.replace_snapshots_batch(&entries)?;
    run.analyzed += entries.len();
    for dependent in api_dependents {
        if !reanalyze.iter().any(|source| source.id == dependent) {
            store.remove_snapshot(&dependent)?;
            run.removed += 1;
        }
    }
    run.source_commit_millis += commit_started.elapsed().as_millis();
    let source = reanalyze.first().expect("non-empty batch");
    run.batches.push(BatchIndexMetrics {
        component: source.component.as_str().to_owned(),
        language: format!("{:?}", source.language).to_lowercase(),
        source_units: reanalyze.len(),
        worker_millis,
        commit_millis: commit_started.elapsed().as_millis(),
        worker_phases,
        worker_metrics,
    });
    tracing::debug!(
        target: "kide::index",
        source_units = reanalyze.len(),
        commit_millis = commit_started.elapsed().as_millis(),
        "source batch committed"
    );
    store.put_manifest(manifest)?;
    run.worker_starts = supervisor.start_count();
    Ok(run)
}

fn index_dependency_catalog(
    store: &mut IndexStore,
    supervisor: &mut WorkerSupervisor,
    artifact_candidates: Vec<ArtifactCandidate>,
) -> Result<(), IndexOrchestratorError> {
    let mut cursor = None;
    let mut page = 0_u64;
    let mut descriptors = BTreeMap::new();
    loop {
        let response = supervisor.request(WorkerEnvelope::new(
            format!("index-artifact-descriptors-{page}"),
            WorkerMessage::ArtifactDiscoveryRequest(ArtifactDiscoveryRequest {
                workspace_root: WorkspacePath::new("."),
                max_artifacts: 64,
                cursor: cursor.clone(),
                artifact_candidates: artifact_candidates.clone(),
            }),
        ))?;
        let WorkerMessage::ArtifactDiscoveryResponse(response) = response.message else {
            return Err(IndexOrchestratorError::InvalidResponse {
                received: Box::new(response.message),
            });
        };
        for descriptor in response.artifacts {
            descriptors.insert(descriptor.source_unit.id.as_str().to_owned(), descriptor);
        }
        match response.next_cursor {
            Some(next) => {
                cursor = Some(next);
                page += 1
            }
            None => {
                store
                    .replace_artifact_descriptors(&descriptors.into_values().collect::<Vec<_>>())?;
                return Ok(());
            }
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
