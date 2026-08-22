use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use kide_core::{
    ArtifactBlobCache, ArtifactBlobKey, BuildSystem, CANONICAL_SCHEMA_VERSION,
    EffectiveConfiguration, IndexStore, ProjectManifestRequest, ProjectManifestResponse,
    Provenance, QueryStatus, WorkerCapability, WorkerEnvelope, WorkerInstallation, WorkerLaunch,
    WorkerMessage, WorkerRegistry, WorkerSupervisor, WorkspaceDiscovery, WorkspacePath,
    cache_catalog_artifact_with_metrics, collect_workspace_text,
    index_batch_with_artifact_cache_provenance_and_execution_plan,
    index_selected_batches_with_execution_plan,
};

pub(super) fn index(
    discovery: WorkspaceDiscovery,
    configuration: EffectiveConfiguration,
    artifact_cache_root: PathBuf,
    verbosity: u8,
    force: bool,
    warm_dependencies: Option<u32>,
    materialize_only: bool,
) -> Result<QueryStatus> {
    let discovery_started = Instant::now();
    tracing::debug!(target: "kide::cli", workspace = %discovery.root.display(), "indexing discovered workspace");
    let discovery_millis = discovery_started.elapsed().as_millis();
    tracing::debug!(target: "kide::cli", "opening index");
    let mut store = IndexStore::open(IndexStore::default_path(&discovery.root))?;
    let configuration_input_records = discovery.configuration_input_records.clone();
    let configuration_changes = store.configuration_input_status(&configuration_input_records)?;
    let mut sources = discovery.source_units;
    if force {
        tracing::info!(target: "kide::cli", sources = sources.len(), "forcing source reanalysis");
        for source in &sources {
            store.remove_snapshot(&source.id)?;
        }
    }
    let artifact_cache = ArtifactBlobCache::open(artifact_cache_root)?;
    let staging = discovery.root.join(".kide/staging");
    std::fs::create_dir_all(&staging)?;
    if materialize_only {
        let warm_limit = warm_dependencies.expect("clap requires warm dependencies");
        let descriptors = store.artifact_descriptors()?;
        let mut worker =
            WorkerSupervisor::new(kotlin_worker_installation(&discovery.root, verbosity)?.launch);
        worker.handshake("materialize-only")?;
        let started = Instant::now();
        let mut budget = kide_core::MaterializationBudget {
            remaining_artifacts: if warm_limit == 0 {
                u32::MAX
            } else {
                warm_limit
            },
        };
        let mut materialized = 0_usize;
        let mut cached = 0_usize;
        let mut artifacts = Vec::new();
        for descriptor in descriptors {
            let artifact_started = Instant::now();
            let (outcome, metrics) = cache_catalog_artifact_with_metrics(
                &mut store,
                &mut worker,
                &artifact_cache,
                discovery.manifest.root.clone(),
                &descriptor.source_unit.id,
                None,
                &staging,
                &mut budget,
            )?;
            match outcome {
                kide_core::MaterializationOutcome::Materialized => {
                    materialized += 1;
                    let blob_bytes = metrics
                        .as_ref()
                        .and_then(|metrics| metrics.worker_metrics.get("artifact_blob_bytes"))
                        .copied()
                        .unwrap_or_default();
                    artifacts.push(serde_json::json!({"source_unit": descriptor.source_unit.id, "outcome": "materialized", "millis": artifact_started.elapsed().as_millis(), "blob_bytes": blob_bytes, "worker_phases": metrics.as_ref().map(|metrics| &metrics.worker_phases), "worker_metrics": metrics.as_ref().map(|metrics| &metrics.worker_metrics)}));
                }
                kide_core::MaterializationOutcome::AlreadyMaterialized => {
                    cached += 1;
                    artifacts.push(serde_json::json!({"source_unit": descriptor.source_unit.id, "outcome": "cached", "millis": artifact_started.elapsed().as_millis()}));
                }
                kide_core::MaterializationOutcome::BudgetExhausted => break,
                kide_core::MaterializationOutcome::NotCataloged => {}
            }
        }
        println!(
            "{}",
            serde_json::json!({
                "schema_version": CANONICAL_SCHEMA_VERSION,
                "status": "ok",
                "workspace": discovery.root,
                "materialization_only": true,
                "cataloged": store.artifact_descriptors()?.len(),
                "materialized": materialized,
                "cached": cached,
                "worker_starts": worker.start_count(),
                "timing_millis": { "dependency_materialization": started.elapsed().as_millis() },
                "artifacts": artifacts,
            })
        );
        return Ok(QueryStatus::Ok);
    }
    let worker_installation = kotlin_worker_installation(&discovery.root, verbosity)?;
    let resolved_plan = resolve_project_manifest(&worker_installation.launch)?;
    let manifest = resolved_plan.manifest;
    let execution_plan = resolved_plan.execution_plan;
    if sources.iter().any(|source| {
        !manifest
            .components
            .iter()
            .any(|component| component.id == source.component)
    }) {
        bail!("resolved_manifest_does_not_cover_discovered_source_components");
    }
    for source in &mut sources {
        source.context = manifest
            .components
            .iter()
            .find(|component| component.id == source.component)
            .expect("coverage checked above")
            .configuration
            .clone();
    }
    let registry = WorkerRegistry::new(vec![worker_installation]);
    let selection = registry.select_with_batch_limit(
        &manifest,
        sources.clone(),
        &[WorkerCapability::FileAnalysisSnapshot],
        source_batch_limit()?,
    )?;
    if !selection.unsupported.is_empty() {
        let unsupported = selection
            .unsupported
            .iter()
            .map(|batch| serde_json::json!({
                "component": batch.component.as_str(),
                "language": batch.language,
                "build_system": batch.build_system,
                "source_units": batch.source_units.iter().map(|source| source.path.as_str()).collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>();
        bail!(
            "unsupported_worker_batches={}",
            serde_json::to_string(&unsupported)?
        );
    }
    tracing::info!(
        target: "kide::index",
        sources = sources.len(),
        batches = selection.batches.len(),
        "starting source analysis phase"
    );
    let mut run = if selection.batches.len() == 1 {
        // Retain the cache-aware dependency catalog path for the common
        // single-backend workspace. Mixed workspaces use the global planner so
        // one batch cannot invalidate another language's source snapshots.
        index_batch_with_artifact_cache_provenance_and_execution_plan(
            &mut store,
            &manifest,
            &sources,
            selection.batches[0].worker.installation.launch.clone(),
            &artifact_cache,
            &staging,
            Provenance {
                backend: selection.batches[0]
                    .worker
                    .capabilities
                    .identity
                    .backend
                    .clone(),
                backend_version: selection.batches[0]
                    .worker
                    .capabilities
                    .identity
                    .backend_version
                    .clone(),
                protocol_version: selection.batches[0].worker.capabilities.protocol_version,
                // JVM snapshots bind analysis options to their canonical
                // component context. Compare that same owner identity on the
                // next run; the workspace manifest remains the resolution
                // barrier, not a per-file worker option.
                analysis_options: sources
                    .first()
                    .expect("a selected batch has sources")
                    .context
                    .clone(),
            },
            execution_plan,
        )?
    } else {
        index_selected_batches_with_execution_plan(
            &mut store,
            &manifest,
            &sources,
            &selection,
            execution_plan,
        )?
    };
    let mut materialized_artifacts = Vec::new();
    if let Some(warm_limit) = warm_dependencies {
        let descriptors = store.artifact_descriptors()?;
        let cataloged = descriptors.len();
        let artifacts_total = if warm_limit == 0 {
            cataloged
        } else {
            cataloged.min(warm_limit as usize)
        };
        tracing::info!(
            target: "kide::index",
            cataloged,
            artifacts_total,
            "starting dependency materialization phase"
        );
        let materialization_started = Instant::now();
        let mut worker =
            WorkerSupervisor::new(selection.batches[0].worker.installation.launch.clone());
        worker.handshake("warm-dependency-cache")?;
        let mut budget = kide_core::MaterializationBudget {
            remaining_artifacts: if warm_limit == 0 {
                u32::MAX
            } else {
                warm_limit
            },
        };
        for (artifact_index, descriptor) in descriptors.into_iter().enumerate() {
            let artifact_index = artifact_index + 1;
            let artifact_started = Instant::now();
            let artifact_locator = run.take_artifact_locator(&descriptor.source_unit.id);
            let dependency = artifact_locator
                .as_deref()
                .and_then(|locator| Path::new(locator).file_name())
                .and_then(|name| name.to_str())
                .unwrap_or(descriptor.source_unit.path.as_str())
                .to_owned();
            let (outcome, metrics) = cache_catalog_artifact_with_metrics(
                &mut store,
                &mut worker,
                &artifact_cache,
                manifest.root.clone(),
                &descriptor.source_unit.id,
                artifact_locator,
                &staging,
                &mut budget,
            )?;
            let (outcome_name, blob_bytes) = match outcome {
                kide_core::MaterializationOutcome::Materialized => {
                    run.dependency_analyzed += 1;
                    if let Some(metrics) = &metrics {
                        run.record_materialization_metrics(metrics);
                    }
                    let key = ArtifactBlobKey::new(
                        descriptor.source_unit.content.clone(),
                        descriptor.source_unit.context.clone(),
                        &descriptor.provenance,
                    );
                    let blob_bytes = artifact_cache
                        .load(&key)?
                        .map(|bytes| bytes.len())
                        .unwrap_or_default();
                    materialized_artifacts.push(serde_json::json!({"source_unit": descriptor.source_unit.id, "component": descriptor.source_unit.component, "dependency": dependency, "outcome": "materialized", "millis": artifact_started.elapsed().as_millis(), "blob_bytes": blob_bytes, "worker_phases": metrics.as_ref().map(|metrics| &metrics.worker_phases), "worker_metrics": metrics.as_ref().map(|metrics| &metrics.worker_metrics)}));
                    ("materialized", blob_bytes)
                }
                kide_core::MaterializationOutcome::AlreadyMaterialized => {
                    run.dependency_reused += 1;
                    if let Some(metrics) = &metrics {
                        run.record_materialization_metrics(metrics);
                    }
                    materialized_artifacts.push(serde_json::json!({"source_unit": descriptor.source_unit.id, "component": descriptor.source_unit.component, "dependency": dependency, "outcome": "cached", "millis": artifact_started.elapsed().as_millis()}));
                    ("cached", 0)
                }
                kide_core::MaterializationOutcome::BudgetExhausted => break,
                _ => continue,
            };
            if verbosity > 1 {
                tracing::info!(
                    target: "kide::index",
                    artifact_index,
                    artifacts_total,
                    component = descriptor.source_unit.component.as_str(),
                    dependency,
                    outcome = outcome_name,
                    elapsed_millis = artifact_started.elapsed().as_millis(),
                    worker_millis = metrics
                        .as_ref()
                        .and_then(|metrics| metrics.worker_phases.get("artifact_total"))
                        .copied()
                        .unwrap_or_default(),
                    checksum_millis = metrics
                        .as_ref()
                        .and_then(|metrics| metrics.core_phases.get("core_staged_checksum"))
                        .copied()
                        .unwrap_or_default(),
                    publish_millis = metrics
                        .as_ref()
                        .and_then(|metrics| metrics.core_phases.get("core_cache_publish"))
                        .copied()
                        .unwrap_or_default(),
                    blob_bytes,
                    "dependency artifact processed"
                );
            }
        }
        run.clear_artifact_locators();
        run.worker_starts += worker.start_count();
        run.dependency_materialization_millis += materialization_started.elapsed().as_millis();
        tracing::info!(
            target: "kide::index",
            materialized = run.dependency_analyzed,
            cached = run.dependency_reused,
            elapsed_millis = run.dependency_materialization_millis,
            "dependency materialization phase complete"
        );
    }
    let text_inventory = collect_workspace_text(&discovery.root)?;
    store.sync_text_documents(&text_inventory.documents)?;
    store.replace_configuration_inputs(&configuration_input_records)?;
    let mut slowest_materializations = materialized_artifacts.clone();
    slowest_materializations.sort_by_key(|artifact| {
        std::cmp::Reverse(
            artifact
                .get("millis")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default(),
        )
    });
    slowest_materializations.truncate(10);
    tracing::info!(
        target: "kide::index",
        analyzed = run.analyzed,
        reused = run.reused,
        removed = run.removed,
        worker_starts = run.worker_starts,
        dependency_analyzed = run.dependency_analyzed,
        discovery_millis,
        source_worker_millis = run.source_worker_millis,
        source_commit_millis = run.source_commit_millis,
        dependency_catalog_millis = run.dependency_catalog_millis,
        dependency_materialization_millis = run.dependency_materialization_millis,
        worker_phases = ?run.worker_phase_millis,
        worker_metrics = ?run.worker_metrics,
        "index complete"
    );
    if verbosity > 0 {
        let import = run
            .worker_phase_millis
            .get("context_import")
            .copied()
            .unwrap_or_default();
        let stage = run
            .worker_phase_millis
            .get("stage_compile")
            .copied()
            .unwrap_or_default();
        let pipeline = run
            .worker_phase_millis
            .get("shard_analyze")
            .copied()
            .unwrap_or_default();
        let semantic = pipeline.saturating_sub(stage);
        let total = run
            .worker_phase_millis
            .get("worker_total")
            .copied()
            .unwrap_or_default();
        let phase_order = [
            "context_import",
            "stage_compile",
            "shard_analyze",
            "serialize",
            "worker_total",
        ];
        let phases = run
            .worker_phase_millis
            .iter()
            .filter(|(phase, _)| !phase_order.contains(&phase.as_str()))
            .map(|(phase, millis)| format!("{phase}={millis}ms"))
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!(
            "index timings\n  discovery: {discovery_millis}ms\n  worker:    {}ms\n  commit:    {}ms\n  batches:   {}\n  worker phases\n    context import: {import}ms\n    shard pipeline: {pipeline}ms\n      stage compile: {stage}ms\n      semantic + facts: {semantic}ms\n    worker total: {total}ms\n  other phases: {}",
            run.source_worker_millis,
            run.source_commit_millis,
            run.batches.len(),
            phases,
        );
        eprintln!(
            "  dependency catalog: {}ms\n  dependency materialization: {}ms",
            run.dependency_catalog_millis, run.dependency_materialization_millis
        );
        if verbosity > 1 {
            for (index, batch) in run.batches.iter().enumerate() {
                let import = batch
                    .worker_phases
                    .get("context_import")
                    .copied()
                    .unwrap_or_default();
                let stage = batch
                    .worker_phases
                    .get("stage_compile")
                    .copied()
                    .unwrap_or_default();
                let pipeline = batch
                    .worker_phases
                    .get("shard_analyze")
                    .copied()
                    .unwrap_or_default();
                eprintln!(
                    "  batch #{index}: {} {} files={} worker={}ms commit={}ms [import={}ms stage={}ms semantic={}ms parse={}ms analyze={}ms facts={}ms cache={}/{} stage-ok={} failed={} unresolved={} response={}B serialize={}ms]",
                    batch.component,
                    batch.language,
                    batch.source_units,
                    batch.worker_millis,
                    batch.commit_millis,
                    import,
                    stage,
                    pipeline.saturating_sub(stage),
                    batch
                        .worker_metrics
                        .get("semantic_parse")
                        .copied()
                        .unwrap_or_default(),
                    batch
                        .worker_metrics
                        .get("semantic_analyze")
                        .copied()
                        .unwrap_or_default(),
                    batch
                        .worker_metrics
                        .get("semantic_facts")
                        .copied()
                        .unwrap_or_default(),
                    batch
                        .worker_metrics
                        .get("stage_cache_hits")
                        .copied()
                        .unwrap_or_default(),
                    batch
                        .worker_metrics
                        .get("stage_cache_misses")
                        .copied()
                        .unwrap_or_default(),
                    batch
                        .worker_metrics
                        .get("stage_compile_success")
                        .copied()
                        .unwrap_or_default(),
                    batch
                        .worker_metrics
                        .get("stage_compile_failed")
                        .copied()
                        .unwrap_or_default(),
                    batch
                        .worker_metrics
                        .get("unresolved_dependencies")
                        .copied()
                        .unwrap_or_default(),
                    batch
                        .worker_metrics
                        .get("response_bytes")
                        .copied()
                        .unwrap_or_default(),
                    batch
                        .worker_metrics
                        .get("serialize_millis")
                        .copied()
                        .unwrap_or_default(),
                );
            }
            if !materialized_artifacts.is_empty() {
                eprintln!(
                    "  dependency materialization: artifacts={} extract={}ms encode={}ms write={}ms worker={}ms checksum={}ms publish={}ms verify={}ms input={}B blob={}B",
                    materialized_artifacts.len(),
                    run.worker_phase_millis
                        .get("artifact_extract")
                        .copied()
                        .unwrap_or_default(),
                    run.worker_phase_millis
                        .get("artifact_encode")
                        .copied()
                        .unwrap_or_default(),
                    run.worker_phase_millis
                        .get("artifact_stage_write")
                        .copied()
                        .unwrap_or_default(),
                    run.worker_phase_millis
                        .get("artifact_total")
                        .copied()
                        .unwrap_or_default(),
                    run.worker_phase_millis
                        .get("core_staged_checksum")
                        .copied()
                        .unwrap_or_default(),
                    run.worker_phase_millis
                        .get("core_cache_publish")
                        .copied()
                        .unwrap_or_default(),
                    run.worker_phase_millis
                        .get("core_staged_verify")
                        .copied()
                        .unwrap_or_default(),
                    run.worker_metrics
                        .get("artifact_input_bytes")
                        .copied()
                        .unwrap_or_default(),
                    run.worker_metrics
                        .get("artifact_blob_bytes")
                        .copied()
                        .unwrap_or_default(),
                );
                for artifact in &slowest_materializations {
                    eprintln!(
                        "    slow dependency: {} {} ({}) {}ms {}B",
                        artifact["component"].as_str().unwrap_or("unknown"),
                        artifact["dependency"].as_str().unwrap_or("unknown"),
                        artifact["source_unit"].as_str().unwrap_or("unknown"),
                        artifact["millis"].as_u64().unwrap_or_default(),
                        artifact["blob_bytes"].as_u64().unwrap_or_default(),
                    );
                }
            }
        }
    }
    println!(
        "{}",
        serde_json::json!({
            "schema_version": CANONICAL_SCHEMA_VERSION,
            "status": "ok",
            "freshness_strategy": configuration.freshness_strategy,
            "worker_limits": configuration.workers,
            "resolved_manifest": if verbosity > 1 { serde_json::to_value(&manifest)? } else { serde_json::Value::Null },
            "workspace": discovery.root,
            "reused": run.reused,
            "analyzed": run.analyzed,
        "removed": run.removed,
        "worker_starts": run.worker_starts,
        "dependency_analyzed": run.dependency_analyzed,
        "dependency_reused": run.dependency_reused,
        "dependency_warm_limit": warm_dependencies.map(|limit| if limit == 0 { "all".to_owned() } else { limit.to_string() }),
        "dependency_materialization_top": if verbosity > 1 { slowest_materializations.clone() } else { Vec::new() },
        "text_documents": text_inventory.documents.len(),
        "text_skipped": text_inventory.skipped.len(),
        "configuration_inputs_changed": configuration_changes.inputs.iter().filter(|input| input.state != kide_core::ConfigurationInputState::Current).count(),
        "affected_components": configuration_changes.affected_components,
        "timing_millis": {
            "discovery": discovery_millis,
            "source_worker": run.source_worker_millis,
            "source_commit": run.source_commit_millis,
            "dependency_catalog": run.dependency_catalog_millis,
            "dependency_materialization": run.dependency_materialization_millis,
            "worker_phases": run.worker_phase_millis,
            "worker_metrics": run.worker_metrics,
            "batches": run.batches,
        },
        })
    );
    Ok(QueryStatus::Ok)
}

fn resolve_project_manifest(launch: &WorkerLaunch) -> Result<ProjectManifestResponse> {
    let mut worker = WorkerSupervisor::new(launch.clone());
    worker.handshake("index-build-resolution")?;
    let response = worker.request(WorkerEnvelope::new(
        "index-project-manifest",
        WorkerMessage::ProjectManifestRequest(ProjectManifestRequest {
            workspace_root: WorkspacePath::new("."),
        }),
    ))?;
    match response.message {
        WorkerMessage::ProjectManifestResponse(response) => Ok(response),
        received => bail!("invalid_project_manifest_response={received:?}"),
    }
}

/// Temporary external scheduling override. Workspace configuration will own
/// this same policy once the config schema includes index execution limits.
fn source_batch_limit() -> Result<usize> {
    match std::env::var("KIDE_MAX_SOURCE_UNITS_PER_WORKER_BATCH") {
        Ok(value) => value
            .parse::<usize>()
            .ok()
            .filter(|limit| *limit > 0)
            .ok_or_else(|| {
                anyhow::anyhow!("KIDE_MAX_SOURCE_UNITS_PER_WORKER_BATCH must be a positive integer")
            }),
        Err(std::env::VarError::NotPresent) => {
            Ok(kide_core::DEFAULT_MAX_SOURCE_UNITS_PER_WORKER_BATCH)
        }
        Err(error) => Err(error.into()),
    }
}

pub(super) fn kotlin_worker_installation(
    workspace: &Path,
    verbosity: u8,
) -> Result<WorkerInstallation> {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let worker = repository.join("workers/kotlin-jvm");
    let executable = worker.join("build/install/kide-kotlin-jvm-worker/bin/kide-kotlin-jvm-worker");
    if !executable.is_file() {
        bail!(
            "Kotlin worker distribution is missing at {}; run `make build` before indexing",
            executable.display()
        );
    }
    let mut launch = WorkerLaunch::new(executable);
    launch.args = vec![OsString::from("--serve")];
    launch.environment.insert(
        OsString::from("KIDE_WORKSPACE_ROOT"),
        workspace.as_os_str().to_os_string(),
    );
    if let Some(gradle_home) = std::env::var_os("GRADLE_HOME") {
        launch
            .environment
            .insert(OsString::from("KIDE_GRADLE_INSTALLATION"), gradle_home);
    }
    launch.environment.insert(
        OsString::from("KIDE_WORKER_LOG_LEVEL"),
        OsString::from(match verbosity {
            0 => "warn",
            1 => "info",
            2 => "info",
            _ => "debug",
        }),
    );
    // A cold K2 batch over a realistic multi-module workspace can exceed the
    // control-plane default while still making progress.
    launch.request_timeout = Duration::from_secs(5 * 60);
    Ok(WorkerInstallation {
        name: "kotlin-jvm".to_owned(),
        launch,
        build_systems: vec![
            BuildSystem::Gradle,
            BuildSystem::Maven,
            BuildSystem::Filesystem,
        ],
    })
}
