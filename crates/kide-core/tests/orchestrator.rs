use std::{ffi::OsString, path::PathBuf, time::Duration};

use kide_core::{
    cache_catalog_artifact, index_batch, index_batch_with_artifact_cache_and_provenance,
    index_selected_batches, materialize_catalog_artifact, ArtifactBlobCache, ArtifactBlobKey,
    ArtifactDescriptor, BuildSystem, Component, ComponentId, DiscoveredWorker, Fingerprint,
    IndexStore, Language, MaterializationBudget, MaterializationOutcome, ProjectManifest,
    Provenance, SourceOrigin, SourceUnit, SourceUnitId, WorkerBatch, WorkerCapabilities,
    WorkerCapability, WorkerIdentity, WorkerInstallation, WorkerLaunch, WorkerSelection,
    WorkerSupervisor, WorkspaceId, WorkspacePath,
};
use tempfile::tempdir;

#[test]
fn indexes_a_cold_batch_then_reuses_unchanged_snapshots() {
    let directory = tempdir().expect("temporary workspace");
    let mut store = IndexStore::open(directory.path().join("index.sqlite3")).expect("opens index");
    let manifest = manifest();
    let sources = vec![
        source("One.kt", "sha256:one"),
        source("Two.kt", "sha256:two"),
    ];

    let first = index_batch(&mut store, &manifest, &sources, launch()).expect("cold index");
    assert_eq!(first.analyzed, 2);
    assert_eq!(first.reused, 0);
    assert_eq!(first.worker_starts, 1);
    assert_eq!(store.source_units().expect("stored units"), sources);

    let second = index_batch(&mut store, &manifest, &sources, launch()).expect("incremental index");
    assert_eq!(second.analyzed, 0);
    assert_eq!(second.reused, 2);
    assert_eq!(second.worker_starts, 0);
}

#[test]
fn selected_source_shards_reuse_one_worker_for_an_index_run() {
    let directory = tempdir().expect("temporary workspace");
    let mut store = IndexStore::open(directory.path().join("index.sqlite3")).expect("opens index");
    let manifest = manifest();
    let mut reusable_launch = launch();
    reusable_launch.idle_timeout = Duration::from_secs(2);
    let worker = discovered_worker(reusable_launch);
    let selection = WorkerSelection {
        batches: vec![
            WorkerBatch {
                worker: worker.clone(),
                component: ComponentId::new("fixture:main"),
                language: Language::Kotlin,
                source_units: vec![source("One.kt", "sha256:one")],
            },
            WorkerBatch {
                worker,
                component: ComponentId::new("fixture:main"),
                language: Language::Kotlin,
                source_units: vec![source("Two.kt", "sha256:two")],
            },
        ],
        unsupported: Vec::new(),
    };

    let run = index_selected_batches(
        &mut store,
        &manifest,
        &[
            source("One.kt", "sha256:one"),
            source("Two.kt", "sha256:two"),
        ],
        &selection,
    )
    .expect("indexes selected shards");

    assert_eq!(run.analyzed, 2);
    assert_eq!(run.worker_starts, 1);
}

#[test]
fn backend_version_drift_reanalyzes_unchanged_sources() {
    let directory = tempdir().expect("temporary workspace");
    let mut store = IndexStore::open(directory.path().join("index.sqlite3")).expect("opens index");
    let manifest = manifest();
    let sources = vec![source("One.kt", "sha256:one")];
    let cache = ArtifactBlobCache::open(directory.path().join("cache")).expect("opens cache");
    let staging = directory.path().join("staging");
    std::fs::create_dir_all(&staging).expect("creates staging");
    let first = index_batch_with_artifact_cache_and_provenance(
        &mut store,
        &manifest,
        &sources,
        launch(),
        &cache,
        &staging,
        worker_provenance(),
    )
    .expect("first index");
    assert_eq!(first.analyzed, 1);
    let mut changed_backend = worker_provenance();
    changed_backend.backend_version = "0.2.0".to_owned();
    let second = index_batch_with_artifact_cache_and_provenance(
        &mut store,
        &manifest,
        &sources,
        launch(),
        &cache,
        &staging,
        changed_backend,
    )
    .expect("backend drift reindexes");
    assert_eq!(second.analyzed, 1);
    assert_eq!(second.reused, 0);
}

#[test]
fn incomplete_worker_batch_keeps_the_previous_source_snapshot() {
    let directory = tempdir().expect("temporary workspace");
    let mut store = IndexStore::open(directory.path().join("index.sqlite3")).expect("opens index");
    let manifest = manifest();
    let original = source("One.kt", "sha256:one");
    index_batch(
        &mut store,
        &manifest,
        std::slice::from_ref(&original),
        launch(),
    )
    .expect("stores initial snapshot");

    let changed = source("One.kt", "sha256:two");
    let error = index_batch(&mut store, &manifest, &[changed], missing_launch())
        .expect_err("rejects incomplete worker batch");
    assert!(error.to_string().contains("did not return a snapshot"));
    assert_eq!(
        store
            .source_unit(&original.id)
            .expect("reads retained snapshot"),
        Some(original)
    );
}

#[test]
fn cached_indexing_catalogs_dependencies_without_eager_graph_materialization() {
    let directory = tempdir().expect("temporary workspace");
    let mut store = IndexStore::open(directory.path().join("index.sqlite3")).expect("opens index");
    let cache = ArtifactBlobCache::open(directory.path().join("cache")).expect("opens cache");
    let staging = directory.path().join("staging");
    std::fs::create_dir_all(&staging).expect("creates staging");
    let descriptor = artifact_descriptor();

    index_batch_with_artifact_cache_and_provenance(
        &mut store,
        &manifest(),
        &[source("One.kt", "sha256:one")],
        materializing_launch(),
        &cache,
        &staging,
        worker_provenance(),
    )
    .expect("indexes source and catalogs dependencies");

    assert_eq!(
        store
            .artifact_descriptor(&descriptor.source_unit.id)
            .expect("reads catalog"),
        Some(descriptor.clone())
    );
    assert!(store
        .source_unit(&descriptor.source_unit.id)
        .expect("reads store")
        .is_none());
    assert!(cache
        .open_blob(&ArtifactBlobKey::new(
            descriptor.source_unit.content,
            &descriptor.provenance,
        ))
        .expect("checks cache")
        .is_none());
}

#[test]
fn cached_indexing_consumes_every_dependency_catalog_page() {
    let directory = tempdir().expect("temporary workspace");
    let mut store = IndexStore::open(directory.path().join("index.sqlite3")).expect("opens index");
    let cache = ArtifactBlobCache::open(directory.path().join("cache")).expect("opens cache");
    let staging = directory.path().join("staging");
    std::fs::create_dir_all(&staging).expect("creates staging");

    index_batch_with_artifact_cache_and_provenance(
        &mut store,
        &manifest(),
        &[source("One.kt", "sha256:one")],
        paginated_launch(),
        &cache,
        &staging,
        worker_provenance(),
    )
    .expect("indexes source and every catalog page");

    assert_eq!(
        store
            .artifact_descriptors()
            .expect("reads catalog")
            .into_iter()
            .map(|descriptor| descriptor.source_unit.id)
            .collect::<Vec<_>>(),
        vec![
            SourceUnitId::new("jvm:sha256:fixture-artifact"),
            SourceUnitId::new("jvm:sha256:fixture-artifact-two"),
        ]
    );
    assert!(store
        .source_units()
        .expect("reads snapshots")
        .iter()
        .all(|source| source.origin != SourceOrigin::Dependency));

    index_batch_with_artifact_cache_and_provenance(
        &mut store,
        &manifest(),
        &[],
        paginated_launch(),
        &cache,
        &staging,
        worker_provenance(),
    )
    .expect("removes source without touching dependency catalog");
    assert_eq!(
        store
            .artifact_descriptors()
            .expect("preserves dependency catalog")
            .len(),
        2
    );
}

#[test]
fn demand_materializes_one_cataloged_artifact_and_respects_explicit_bounds() {
    let directory = tempdir().expect("temporary workspace");
    let mut store = IndexStore::open(directory.path().join("index.sqlite3")).expect("opens index");
    let artifact = artifact_descriptor();
    store
        .put_artifact_descriptor(&artifact)
        .expect("catalogs artifact");
    assert!(store
        .source_unit(&artifact.source_unit.id)
        .expect("reads store")
        .is_none());

    let cache = ArtifactBlobCache::open(directory.path().join("cache")).expect("opens cache");
    let staging = directory.path().join("staging");
    std::fs::create_dir_all(&staging).expect("creates staging");
    let mut worker = WorkerSupervisor::new(materializing_launch());
    worker.handshake("fixture-handshake").expect("handshakes");
    let mut budget = MaterializationBudget {
        remaining_artifacts: 1,
    };
    assert_eq!(
        materialize_catalog_artifact(
            &mut store,
            &mut worker,
            &cache,
            WorkspacePath::new("."),
            &artifact.source_unit.id,
            &staging,
            &mut budget,
        )
        .expect("materializes catalog artifact"),
        MaterializationOutcome::Materialized,
    );
    assert_eq!(budget.remaining_artifacts, 0);
    assert_eq!(
        store
            .source_unit(&artifact.source_unit.id)
            .expect("reads store"),
        Some(artifact.source_unit.clone())
    );

    let mut bounded_store =
        IndexStore::open(directory.path().join("bounded.sqlite3")).expect("opens bounded index");
    bounded_store
        .put_artifact_descriptor(&artifact_descriptor())
        .expect("catalogs artifact");
    let mut never_started = WorkerSupervisor::new(materializing_launch());
    let mut no_budget = MaterializationBudget {
        remaining_artifacts: 0,
    };
    assert_eq!(
        materialize_catalog_artifact(
            &mut bounded_store,
            &mut never_started,
            &cache,
            WorkspacePath::new("."),
            &artifact.source_unit.id,
            &staging,
            &mut no_budget
        )
        .expect("reports exhausted budget"),
        MaterializationOutcome::BudgetExhausted,
    );
    assert_eq!(
        materialize_catalog_artifact(
            &mut bounded_store,
            &mut never_started,
            &cache,
            WorkspacePath::new("."),
            &SourceUnitId::new("jvm:missing"),
            &staging,
            &mut MaterializationBudget {
                remaining_artifacts: 1
            }
        )
        .expect("reports absent catalog entry"),
        MaterializationOutcome::NotCataloged,
    );
}

#[test]
fn cache_materializes_one_cataloged_artifact_without_sqlite_projection() {
    let directory = tempdir().expect("temporary workspace");
    let store = IndexStore::open(directory.path().join("index.sqlite3")).expect("opens index");
    let artifact = artifact_descriptor();
    store
        .put_artifact_descriptor(&artifact)
        .expect("catalogs artifact");
    let cache = ArtifactBlobCache::open(directory.path().join("cache")).expect("opens cache");
    let staging = directory.path().join("staging");
    std::fs::create_dir_all(&staging).expect("creates staging");
    let mut worker = WorkerSupervisor::new(materializing_launch());
    worker.handshake("fixture-handshake").expect("handshakes");

    assert_eq!(
        cache_catalog_artifact(
            &store,
            &mut worker,
            &cache,
            WorkspacePath::new("."),
            &artifact.source_unit.id,
            None,
            &staging,
            &mut MaterializationBudget {
                remaining_artifacts: 1
            },
        )
        .expect("caches artifact"),
        MaterializationOutcome::Materialized,
    );
    assert!(cache
        .open_blob(&ArtifactBlobKey::new(
            artifact.source_unit.content.clone(),
            &artifact.provenance,
        ))
        .expect("opens cache")
        .is_some());
    assert!(store
        .source_unit(&artifact.source_unit.id)
        .expect("reads store")
        .is_none());
}

fn launch() -> WorkerLaunch {
    let mut launch = WorkerLaunch::new(PathBuf::from(env!("CARGO_BIN_EXE_kide-fixture-worker")));
    launch.args = vec![OsString::from("normal")];
    launch.idle_timeout = Duration::from_millis(10);
    launch.request_timeout = Duration::from_secs(2);
    launch
}

fn discovered_worker(launch: WorkerLaunch) -> DiscoveredWorker {
    DiscoveredWorker {
        installation: WorkerInstallation {
            name: "fixture".to_owned(),
            launch,
            build_systems: vec![BuildSystem::Filesystem],
        },
        capabilities: WorkerCapabilities {
            identity: WorkerIdentity {
                backend: "kide-fixture-worker".to_owned(),
                backend_version: "0.1.0".to_owned(),
            },
            protocol_version: kide_core::WORKER_PROTOCOL_VERSION,
            languages: vec![Language::Kotlin],
            capabilities: vec![
                WorkerCapability::Handshake,
                WorkerCapability::FileAnalysisSnapshot,
            ],
            semantic_query_capabilities: Vec::new(),
        },
    }
}

fn materializing_launch() -> WorkerLaunch {
    let mut launch = launch();
    launch.args = vec![OsString::from("materialize")];
    launch
}

fn paginated_launch() -> WorkerLaunch {
    let mut launch = launch();
    launch.args = vec![OsString::from("paginated")];
    launch
}

fn missing_launch() -> WorkerLaunch {
    let mut launch = launch();
    launch.args = vec![OsString::from("missing")];
    launch
}

fn artifact_descriptor() -> ArtifactDescriptor {
    ArtifactDescriptor {
        source_unit: SourceUnit {
            id: SourceUnitId::new("jvm:sha256:fixture-artifact"),
            component: ComponentId::new("fixture:main"),
            path: WorkspacePath::new(".kide/dependencies/fixture"),
            language: Language::Java,
            origin: SourceOrigin::Dependency,
            content: Fingerprint::new("sha256:fixture-artifact"),
            context: Fingerprint::new("sha256:fixture-context"),
        },
        provenance: Provenance {
            backend: "kide-fixture-worker".to_owned(),
            backend_version: "0.1.0".to_owned(),
            protocol_version: kide_core::WORKER_PROTOCOL_VERSION,
            analysis_options: Fingerprint::new("sha256:fixture"),
        },
        symbol_locators: Vec::new(),
    }
}

fn manifest() -> ProjectManifest {
    ProjectManifest {
        workspace: WorkspaceId::new("workspace:fixture"),
        root: WorkspacePath::new("."),
        components: vec![Component {
            id: ComponentId::new("fixture:main"),
            name: "fixture".to_owned(),
            build_system: BuildSystem::Filesystem,
            root: WorkspacePath::new("."),
            languages: vec![Language::Kotlin],
            configuration: Fingerprint::new("sha256:project"),
            source_sets: Vec::new(),
            classpath: Vec::new(),
            toolchain: None,
            compiler_configuration: None,
        }],
        dependencies: Vec::new(),
        fingerprint: Fingerprint::new("sha256:project"),
        provenance: provenance(),
    }
}

fn source(name: &str, content: &str) -> SourceUnit {
    SourceUnit {
        id: SourceUnitId::new(format!("fixture:{name}")),
        component: ComponentId::new("fixture:main"),
        path: WorkspacePath::new(format!("src/{name}")),
        language: Language::Kotlin,
        origin: SourceOrigin::Source,
        content: Fingerprint::new(content),
        context: Fingerprint::new("sha256:context"),
    }
}

fn provenance() -> Provenance {
    Provenance {
        backend: "fixture".to_owned(),
        backend_version: "0.1.0".to_owned(),
        protocol_version: kide_core::WORKER_PROTOCOL_VERSION,
        analysis_options: Fingerprint::new("sha256:project"),
    }
}

fn worker_provenance() -> Provenance {
    Provenance {
        backend: "kide-fixture-worker".to_owned(),
        backend_version: "0.1.0".to_owned(),
        protocol_version: kide_core::WORKER_PROTOCOL_VERSION,
        analysis_options: Fingerprint::new("sha256:fixture"),
    }
}
