use std::{ffi::OsString, path::PathBuf, time::Duration};

use kide_core::{
    BuildSystem, Component, ComponentId, Fingerprint, IndexStore, Language, ProjectManifest,
    Provenance, SourceOrigin, SourceUnit, SourceUnitId, WorkerLaunch, WorkspaceId, WorkspacePath,
    index_batch,
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

fn launch() -> WorkerLaunch {
    let mut launch = WorkerLaunch::new(PathBuf::from(env!("CARGO_BIN_EXE_kide-fixture-worker")));
    launch.args = vec![OsString::from("normal")];
    launch.idle_timeout = Duration::from_millis(10);
    launch.request_timeout = Duration::from_secs(2);
    launch
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
