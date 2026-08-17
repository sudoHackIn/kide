use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Result, bail};
use kide_core::{
    ArtifactBlobCache, BuildSystem, CANONICAL_SCHEMA_VERSION, IndexStore, Provenance, QueryStatus,
    WorkerCapability, WorkerInstallation, WorkerLaunch, WorkerRegistry, collect_workspace_text,
    discover_workspace, index_batch_with_artifact_cache_and_provenance, index_selected_batches,
};

pub(super) fn index(path: PathBuf, verbosity: u8, force: bool) -> Result<QueryStatus> {
    tracing::debug!(target: "kide::cli", workspace = %path.display(), "discovering workspace");
    let discovery = discover_workspace(&path)?;
    tracing::debug!(target: "kide::cli", "opening index");
    let mut store = IndexStore::open(IndexStore::default_path(&discovery.root))?;
    let sources = discovery.source_units;
    if force {
        tracing::info!(target: "kide::cli", sources = sources.len(), "forcing source reanalysis");
        for source in &sources {
            store.remove_snapshot(&source.id)?;
        }
    }
    let artifact_cache_root = std::env::var_os("KIDE_ARTIFACT_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| discovery.root.join(".kide/artifact-cache"));
    let artifact_cache = ArtifactBlobCache::open(artifact_cache_root)?;
    let staging = discovery.root.join(".kide/staging");
    std::fs::create_dir_all(&staging)?;
    let registry = WorkerRegistry::new(vec![kotlin_worker_installation(
        &discovery.root,
        verbosity,
    )?]);
    let selection = registry.select(
        &discovery.manifest,
        sources.clone(),
        &[WorkerCapability::FileAnalysisSnapshot],
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
    let run = if selection.batches.len() == 1 {
        // Retain the cache-aware dependency catalog path for the common
        // single-backend workspace. Mixed workspaces use the global planner so
        // one batch cannot invalidate another language's source snapshots.
        index_batch_with_artifact_cache_and_provenance(
            &mut store,
            &discovery.manifest,
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
                analysis_options: discovery.manifest.fingerprint.clone(),
            },
        )?
    } else {
        index_selected_batches(&mut store, &discovery.manifest, &sources, &selection)?
    };
    let text_inventory = collect_workspace_text(&discovery.root)?;
    store.sync_text_documents(&text_inventory.documents)?;
    tracing::info!(target: "kide::cli", analyzed = run.analyzed, dependency_analyzed = run.dependency_analyzed, "index complete");
    println!(
        "{}",
        serde_json::json!({
            "schema_version": CANONICAL_SCHEMA_VERSION,
            "status": "ok",
            "workspace": discovery.root,
            "reused": run.reused,
            "analyzed": run.analyzed,
        "removed": run.removed,
        "worker_starts": run.worker_starts,
        "dependency_analyzed": run.dependency_analyzed,
        "dependency_reused": run.dependency_reused,
        "text_documents": text_inventory.documents.len(),
        "text_skipped": text_inventory.skipped.len(),
        })
    );
    Ok(QueryStatus::Ok)
}

fn kotlin_worker_installation(workspace: &Path, verbosity: u8) -> Result<WorkerInstallation> {
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
    launch.environment.insert(
        OsString::from("KIDE_WORKER_LOG_LEVEL"),
        OsString::from(match verbosity {
            0 => "warn",
            1 => "info",
            _ => "debug",
        }),
    );
    // A cold K2 batch over a realistic multi-module workspace can exceed the
    // control-plane default while still making progress.
    launch.request_timeout = Duration::from_secs(5 * 60);
    Ok(WorkerInstallation {
        name: "kotlin-jvm".to_owned(),
        launch,
        build_systems: vec![BuildSystem::Gradle, BuildSystem::Filesystem],
    })
}
