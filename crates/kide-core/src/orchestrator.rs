//! Batch orchestration for cold language workers.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    thread,
    time::Instant,
};

use thiserror::Error;

use crate::{
    AnalysisFact, AnalysisInput, AnalyzeBatchRequest, ArtifactBlobCache, ArtifactBlobCacheError,
    ArtifactBlobKey, ArtifactDescriptor, ArtifactDiscoveryRequest, ArtifactMaterializationRequest,
    FileAnalysisSnapshot, Fingerprint, IndexAction, IndexStore, IndexStoreError, ManifestAction,
    OpaqueExecutionPlan, ProjectManifest, Provenance, SourceOrigin, SourceUnit, WorkerBatch,
    WorkerCapability, WorkerEnvelope, WorkerLaunch, WorkerMessage, WorkerSelection,
    WorkerSupervisor, WorkerSupervisorError, WorkspacePath, plan_incremental_analysis_index,
    plan_incremental_index,
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
    artifact_locators: BTreeMap<String, String>,
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

/// Worker-side details for one artifact materialization. Core-side cache
/// promotion is deliberately excluded: it is measured by the caller's wall
/// time, while these values explain the worker's actual artifact work.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MaterializationMetrics {
    pub promoted: bool,
    pub worker_phases: BTreeMap<String, u64>,
    pub worker_metrics: BTreeMap<String, u64>,
    pub core_phases: BTreeMap<String, u64>,
}

impl IndexRun {
    /// Includes worker-side dependency materialization measurements in the
    /// run-level aggregates without treating them as source-analysis time.
    pub fn record_materialization_metrics(&mut self, metrics: &MaterializationMetrics) {
        for (phase, millis) in &metrics.worker_phases {
            *self.worker_phase_millis.entry(phase.clone()).or_default() += millis;
        }
        for (name, value) in &metrics.worker_metrics {
            *self.worker_metrics.entry(name.clone()).or_default() += value;
        }
        for (phase, millis) in &metrics.core_phases {
            *self.worker_phase_millis.entry(phase.clone()).or_default() += millis;
        }
    }

    /// Consumes a worker-local locator. It is intentionally unavailable after
    /// the warmup request and never belongs to persistent index state.
    pub fn take_artifact_locator(&mut self, source_unit: &crate::SourceUnitId) -> Option<String> {
        self.artifact_locators.remove(source_unit.as_str())
    }

    pub fn clear_artifact_locators(&mut self) {
        self.artifact_locators.clear();
    }
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
    #[error(
        "execution plan from {backend} targets {actual:?}, but the resolved manifest is {expected:?}"
    )]
    ExecutionPlanManifestMismatch {
        backend: String,
        actual: Fingerprint,
        expected: Fingerprint,
    },
    #[error("worker {worker} no longer supports batch {component} ({language:?})")]
    WorkerNoLongerCompatible {
        worker: String,
        component: String,
        language: crate::Language,
    },
    #[error("dependency discovery worker panicked")]
    DependencyWorkerPanicked,
    #[error(
        "dependency descriptor for component {component} used context {actual:?}, expected resolved context {expected:?}"
    )]
    DependencyContextMismatch {
        component: String,
        expected: Fingerprint,
        actual: Fingerprint,
    },
    #[error("dependency descriptor belongs to unknown resolved component {component}")]
    UnknownDependencyComponent { component: String },
}

/// The dependency lane returns data only. The coordinator remains the sole
/// owner of SQLite and publishes this catalog after the source lane commits.
#[derive(Debug)]
struct DependencyCatalog {
    descriptors: Vec<ArtifactDescriptor>,
    artifact_locators: BTreeMap<String, String>,
    worker_starts: u64,
    elapsed_millis: u128,
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
    index_selected_batches_with_execution_plan(
        store,
        manifest,
        current,
        selection,
        OpaqueExecutionPlan {
            backend: "legacy".into(),
            resolved_fingerprint: manifest.fingerprint.clone(),
            payload: Vec::new(),
        },
    )
}

/// Runs selected source batches while reusing the transient artifact plan
/// emitted by the build-resolution worker.
pub fn index_selected_batches_with_execution_plan(
    store: &mut IndexStore,
    manifest: &ProjectManifest,
    current: &[SourceUnit],
    selection: &WorkerSelection,
    execution_plan: OpaqueExecutionPlan,
) -> Result<IndexRun, IndexOrchestratorError> {
    validate_execution_plan(manifest, &execution_plan)?;
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
    // Source and dependency lanes have separate invalidation boundaries.  A
    // new resolved manifest can change artifact identities even when every
    // source fingerprint remains reusable.
    let dependency_catalog_needed = store
        .latest_manifest()?
        .is_none_or(|previous| previous.fingerprint != manifest.fingerprint);
    let actions = plan_incremental_index(
        ManifestAction::Reuse,
        current,
        &persisted_sources,
        &[],
        &[],
        |_| false,
    )
    .sources;
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
        artifact_locators: BTreeMap::new(),
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
    // Dependency discovery has no SQLite ownership and is independent of
    // source snapshots once build resolution has completed. Start it before
    // the source lane so its cold worker and resolver work overlap with
    // source analysis. It currently uses one worker because the paginated
    // protocol has no independent artifact partitions yet.
    let dependency_lane = (dependency_catalog_needed || !reanalyze.is_empty()).then(|| {
        let launch = selection
            .batches
            .first()
            .expect("a supported selection has at least one batch")
            .worker
            .installation
            .launch
            .clone();
        let manifest = manifest.clone();
        let dependency_plan = execution_plan.clone();
        thread::spawn(move || discover_dependency_catalog(launch, manifest, dependency_plan))
    });
    let mut supervisors = Vec::<(WorkerLaunch, WorkerSupervisor)>::new();
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
            store,
            manifest,
            batch,
            requested,
            index,
            supervisor,
            &mut run,
            &execution_plan,
        )?;
    }
    if let Some(dependency_lane) = dependency_lane {
        let catalog = dependency_lane
            .join()
            .map_err(|_| IndexOrchestratorError::DependencyWorkerPanicked)??;
        store.replace_artifact_descriptors(&catalog.descriptors)?;
        run.artifact_locators = catalog.artifact_locators;
        run.worker_starts += catalog.worker_starts;
        run.dependency_catalog_millis += catalog.elapsed_millis;
    }
    run.worker_starts += supervisors
        .iter()
        .map(|(_, supervisor)| supervisor.start_count())
        .sum::<u64>();
    store.put_manifest(manifest)?;
    Ok(run)
}

// These are separate scheduler resources; keeping them explicit makes the
// source batch transaction boundary and its ownership visible at each call.
#[allow(clippy::too_many_arguments)]
fn analyze_selected_batch(
    store: &mut IndexStore,
    manifest: &ProjectManifest,
    batch: &WorkerBatch,
    requested: Vec<SourceUnit>,
    batch_index: usize,
    supervisor: &mut WorkerSupervisor,
    run: &mut IndexRun,
    execution_plan: &OpaqueExecutionPlan,
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
            execution_plan: execution_plan.clone(),
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
    artifact_locator: Option<String>,
    staging_directory: &Path,
    request_id: impl Into<String>,
) -> Result<MaterializationMetrics, IndexOrchestratorError> {
    let request_id = request_id.into();
    let _span = tracing::info_span!(
        target: "kide::dependency",
        "materialize_artifact",
        source_unit_id = artifact.source_unit.id.as_str(),
        component = artifact.source_unit.component.as_str(),
        dependency_path = artifact.source_unit.path.as_str(),
    )
    .entered();
    let key = ArtifactBlobKey::for_descriptor(&artifact);
    let cache_check_started = Instant::now();
    let cached = cache_hit_or_remove_invalid(cache, &key)?;
    let cache_check_millis = cache_check_started.elapsed().as_millis() as u64;
    if cached {
        return Ok(MaterializationMetrics {
            core_phases: BTreeMap::from([("core_cache_check".to_owned(), cache_check_millis)]),
            ..MaterializationMetrics::default()
        });
    }
    let response = supervisor.request(WorkerEnvelope::new(
        request_id,
        WorkerMessage::ArtifactMaterializationRequest(Box::new(ArtifactMaterializationRequest {
            workspace_root,
            artifact,
            staging_directory: staging_directory.to_string_lossy().into_owned(),
            blob_format_version: crate::artifact_blob_layout::ARTIFACT_BLOB_LAYOUT_VERSION,
            artifact_locator,
        })),
    ))?;
    let WorkerMessage::ArtifactMaterializationResponse(response) = response.message else {
        return Err(IndexOrchestratorError::InvalidResponse {
            received: Box::new(response.message),
        });
    };
    let response = *response;
    let verify_started = Instant::now();
    let digest =
        sha256_bytes(&response.sha256).ok_or(IndexOrchestratorError::InvalidStagedArtifact)?;
    let staged = staging_directory.join(&response.staged_filename);
    if staged.file_name().and_then(|name| name.to_str()) != Some(response.staged_filename.as_str())
    {
        return Err(IndexOrchestratorError::InvalidStagedArtifact);
    }
    let verify_millis = verify_started.elapsed().as_millis() as u64;
    let promotion = cache
        .promote_staged_with_metrics(&key, &staged, response.byte_length, digest)
        .map_err(IndexOrchestratorError::from)?;
    // A promoted blob is immutable in the cache. The worker-created file is
    // only a verified hand-off buffer and must not retain a second full copy.
    let cleanup_started = Instant::now();
    std::fs::remove_file(staged).map_err(ArtifactBlobCacheError::from)?;
    let cleanup_millis = cleanup_started.elapsed().as_millis() as u64;
    let worker_phases = response
        .timings
        .into_iter()
        .fold(BTreeMap::new(), |mut phases, timing| {
            *phases.entry(timing.phase).or_default() += timing.elapsed_millis;
            phases
        });
    let worker_metrics =
        response
            .metrics
            .into_iter()
            .fold(BTreeMap::new(), |mut metrics, metric| {
                *metrics.entry(metric.name).or_default() += metric.value;
                metrics
            });
    let metrics = MaterializationMetrics {
        promoted: promotion.promoted,
        worker_phases,
        worker_metrics,
        core_phases: BTreeMap::from([
            ("core_cache_check".to_owned(), cache_check_millis),
            ("core_staged_verify".to_owned(), verify_millis),
            (
                "core_staged_checksum".to_owned(),
                promotion.staged_checksum_millis,
            ),
            (
                "core_cache_publish".to_owned(),
                promotion.cache_publish_millis,
            ),
            ("core_staging_cleanup".to_owned(), cleanup_millis),
        ]),
    };
    tracing::debug!(
        target: "kide::dependency",
        promoted = metrics.promoted,
        worker_phases = ?metrics.worker_phases,
        worker_metrics = ?metrics.worker_metrics,
        core_phases = ?metrics.core_phases,
        "dependency artifact materialized"
    );
    Ok(metrics)
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
    let key = ArtifactBlobKey::for_descriptor(&descriptor);
    let catalog_descriptor = descriptor.clone();
    materialize_artifact(
        supervisor,
        cache,
        workspace_root,
        descriptor.clone(),
        None,
        staging_directory,
        format!("demand-materialize-{}", source_unit.as_str()),
    )?;
    let payload = cache
        .load(&key)?
        .ok_or(IndexOrchestratorError::InvalidStagedArtifact)?;
    store.record_artifact_blob(&catalog_descriptor, payload.len() as u64)?;
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
#[allow(clippy::too_many_arguments)]
pub fn cache_catalog_artifact(
    store: &mut IndexStore,
    supervisor: &mut WorkerSupervisor,
    cache: &ArtifactBlobCache,
    workspace_root: WorkspacePath,
    source_unit: &crate::SourceUnitId,
    artifact_locator: Option<String>,
    staging_directory: &Path,
    budget: &mut MaterializationBudget,
) -> Result<MaterializationOutcome, IndexOrchestratorError> {
    Ok(cache_catalog_artifact_with_metrics(
        store,
        supervisor,
        cache,
        workspace_root,
        source_unit,
        artifact_locator,
        staging_directory,
        budget,
    )?
    .0)
}

/// Same bounded cache warmup operation, retaining worker measurements for the
/// CLI/run aggregator. Cache hits have no worker metrics.
#[allow(clippy::too_many_arguments)]
pub fn cache_catalog_artifact_with_metrics(
    store: &mut IndexStore,
    supervisor: &mut WorkerSupervisor,
    cache: &ArtifactBlobCache,
    workspace_root: WorkspacePath,
    source_unit: &crate::SourceUnitId,
    artifact_locator: Option<String>,
    staging_directory: &Path,
    budget: &mut MaterializationBudget,
) -> Result<(MaterializationOutcome, Option<MaterializationMetrics>), IndexOrchestratorError> {
    let Some(descriptor) = store.artifact_descriptor(source_unit)? else {
        return Ok((MaterializationOutcome::NotCataloged, None));
    };
    let key = ArtifactBlobKey::for_descriptor(&descriptor);
    if cache_hit_or_remove_invalid(cache, &key)? {
        return Ok((MaterializationOutcome::AlreadyMaterialized, None));
    }
    if budget.remaining_artifacts == 0 {
        return Ok((MaterializationOutcome::BudgetExhausted, None));
    }
    budget.remaining_artifacts -= 1;
    let metrics = materialize_artifact(
        supervisor,
        cache,
        workspace_root,
        descriptor.clone(),
        artifact_locator,
        staging_directory,
        format!("cache-materialize-{}", source_unit.as_str()),
    )?;
    let byte_length = cache
        .open_blob(&key)?
        .ok_or(IndexOrchestratorError::InvalidStagedArtifact)?
        .len();
    store.record_artifact_blob(&descriptor, byte_length)?;
    if metrics.promoted {
        Ok((MaterializationOutcome::Materialized, Some(metrics)))
    } else {
        Ok((MaterializationOutcome::AlreadyMaterialized, Some(metrics)))
    }
}

/// A malformed immutable blob is never a cache hit. Delete only the exact
/// key after validation has rejected it, so a worker can atomically publish a
/// verified replacement in this same run.
fn cache_hit_or_remove_invalid(
    cache: &ArtifactBlobCache,
    key: &ArtifactBlobKey,
) -> Result<bool, IndexOrchestratorError> {
    match cache.open_blob(key) {
        Ok(Some(_)) => Ok(true),
        Ok(None) => Ok(false),
        Err(ArtifactBlobCacheError::InvalidHeader)
        | Err(ArtifactBlobCacheError::TruncatedPayload { .. }) => {
            cache.remove_invalid(key)?;
            Ok(false)
        }
        Err(error) => Err(error.into()),
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
    index_batch_with_optional_cache(
        store,
        manifest,
        current,
        launch,
        None,
        None,
        legacy_execution_plan(manifest),
    )
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
        legacy_execution_plan(manifest),
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
        legacy_execution_plan(manifest),
    )
}

/// Same as [`index_batch_with_artifact_cache_and_provenance`], but consumes
/// the ephemeral dependency plan emitted by build resolution instead of
/// asking the dependency lane to resolve Maven/Gradle again.
#[allow(clippy::too_many_arguments)]
pub fn index_batch_with_artifact_cache_provenance_and_execution_plan(
    store: &mut IndexStore,
    manifest: &ProjectManifest,
    current: &[SourceUnit],
    launch: WorkerLaunch,
    cache: &ArtifactBlobCache,
    staging_directory: &Path,
    provenance: Provenance,
    execution_plan: OpaqueExecutionPlan,
) -> Result<IndexRun, IndexOrchestratorError> {
    validate_execution_plan(manifest, &execution_plan)?;
    index_batch_with_optional_cache(
        store,
        manifest,
        current,
        launch,
        Some((cache, staging_directory)),
        Some(provenance),
        execution_plan,
    )
}

fn validate_execution_plan(
    manifest: &ProjectManifest,
    execution_plan: &OpaqueExecutionPlan,
) -> Result<(), IndexOrchestratorError> {
    if execution_plan.resolved_fingerprint != manifest.fingerprint {
        return Err(IndexOrchestratorError::ExecutionPlanManifestMismatch {
            backend: execution_plan.backend.clone(),
            actual: execution_plan.resolved_fingerprint.clone(),
            expected: manifest.fingerprint.clone(),
        });
    }
    Ok(())
}

fn index_batch_with_optional_cache(
    store: &mut IndexStore,
    manifest: &ProjectManifest,
    current: &[SourceUnit],
    launch: WorkerLaunch,
    _artifact_cache: Option<(&ArtifactBlobCache, &Path)>,
    current_provenance: Option<Provenance>,
    execution_plan: OpaqueExecutionPlan,
) -> Result<IndexRun, IndexOrchestratorError> {
    let persisted_sources = store
        .source_units()?
        .into_iter()
        .filter(|source| source.origin != SourceOrigin::Dependency)
        .collect::<Vec<_>>();
    let dependency_catalog_needed = store
        .latest_manifest()?
        .is_none_or(|previous| previous.fingerprint != manifest.fingerprint);
    let actions = if let Some(provenance) = current_provenance {
        plan_incremental_analysis_index(
            ManifestAction::Reuse,
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
            &[],
            &[],
            |_| false,
        )
        .sources
    } else {
        plan_incremental_index(
            ManifestAction::Reuse,
            current,
            &persisted_sources,
            &[],
            &[],
            |_| false,
        )
        .sources
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
        artifact_locators: BTreeMap::new(),
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
    if reanalyze.is_empty() && !dependency_catalog_needed {
        store.put_manifest(manifest)?;
        return Ok(run);
    }
    // Artifact discovery starts from the resolved build model, not from a
    // source batch response. Running it separately lets its resolver overlap
    // the one source worker without handing SQLite to another thread.
    let dependency_lane = dependency_catalog_needed.then(|| {
        thread::spawn({
            let launch = launch.clone();
            let manifest = manifest.clone();
            let dependency_plan = execution_plan.clone();
            move || discover_dependency_catalog(launch, manifest, dependency_plan)
        })
    });
    if reanalyze.is_empty() {
        if let Some(dependency_lane) = dependency_lane {
            let catalog = dependency_lane
                .join()
                .map_err(|_| IndexOrchestratorError::DependencyWorkerPanicked)??;
            store.replace_artifact_descriptors(&catalog.descriptors)?;
            run.artifact_locators = catalog.artifact_locators;
            run.worker_starts += catalog.worker_starts;
            run.dependency_catalog_millis += catalog.elapsed_millis;
        }
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
            execution_plan: execution_plan.clone(),
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
    if let Some(dependency_lane) = dependency_lane {
        let catalog = dependency_lane
            .join()
            .map_err(|_| IndexOrchestratorError::DependencyWorkerPanicked)??;
        store.replace_artifact_descriptors(&catalog.descriptors)?;
        run.artifact_locators = catalog.artifact_locators;
        run.worker_starts += catalog.worker_starts;
        run.dependency_catalog_millis += catalog.elapsed_millis;
    }
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
    run.worker_starts += supervisor.start_count();
    Ok(run)
}

fn legacy_execution_plan(manifest: &ProjectManifest) -> OpaqueExecutionPlan {
    OpaqueExecutionPlan {
        backend: "legacy".into(),
        resolved_fingerprint: manifest.fingerprint.clone(),
        payload: Vec::new(),
    }
}

fn discover_dependency_catalog(
    launch: WorkerLaunch,
    manifest: ProjectManifest,
    execution_plan: OpaqueExecutionPlan,
) -> Result<DependencyCatalog, IndexOrchestratorError> {
    let started = Instant::now();
    let mut supervisor = WorkerSupervisor::new(launch);
    supervisor.handshake("index-dependency-catalog")?;
    tracing::info!(
        target: "kide::index",
        backend = execution_plan.backend,
        "starting dependency catalog phase"
    );
    let mut cursor = None;
    let mut page = 0_u64;
    let expected_contexts = manifest
        .components
        .into_iter()
        .map(|component| (component.id.as_str().to_owned(), component.configuration))
        .collect::<BTreeMap<_, _>>();
    let mut descriptors = BTreeMap::new();
    let mut artifact_locators = BTreeMap::new();
    loop {
        let response = supervisor.request(WorkerEnvelope::new(
            format!("index-artifact-descriptors-{page}"),
            WorkerMessage::ArtifactDiscoveryRequest(ArtifactDiscoveryRequest {
                workspace_root: WorkspacePath::new("."),
                max_artifacts: 64,
                cursor: cursor.clone(),
                execution_plan: execution_plan.clone(),
            }),
        ))?;
        let WorkerMessage::ArtifactDiscoveryResponse(response) = response.message else {
            return Err(IndexOrchestratorError::InvalidResponse {
                received: Box::new(response.message),
            });
        };
        for descriptor in response.artifacts {
            let expected = expected_contexts
                .get(descriptor.source_unit.component.as_str())
                .ok_or_else(|| IndexOrchestratorError::UnknownDependencyComponent {
                    component: descriptor.source_unit.component.as_str().to_owned(),
                })?;
            if descriptor.source_unit.context != *expected {
                return Err(IndexOrchestratorError::DependencyContextMismatch {
                    component: descriptor.source_unit.component.as_str().to_owned(),
                    expected: expected.clone(),
                    actual: descriptor.source_unit.context.clone(),
                });
            }
            descriptors.insert(descriptor.source_unit.id.as_str().to_owned(), descriptor);
        }
        for locator in response.artifact_locators {
            artifact_locators.insert(locator.source_unit.as_str().to_owned(), locator.locator);
        }
        match response.next_cursor {
            Some(next) => {
                cursor = Some(next);
                page += 1
            }
            None => {
                let artifacts = descriptors.len();
                tracing::info!(
                    target: "kide::index",
                    artifacts,
                    pages = page + 1,
                    "dependency catalog phase complete"
                );
                return Ok(DependencyCatalog {
                    descriptors: descriptors.into_values().collect(),
                    artifact_locators,
                    worker_starts: supervisor.start_count(),
                    elapsed_millis: started.elapsed().as_millis(),
                });
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
