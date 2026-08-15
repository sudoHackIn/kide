use std::{ffi::OsString, path::PathBuf, time::Duration};

use kide_core::{
    AnalysisFact, AnalyzeBatchRequest, ArtifactBlobCache, ArtifactDescriptor, ComponentId,
    Fingerprint, Language, SourceOrigin, SourceUnit, SourceUnitId, WorkerEnvelope, WorkerLaunch,
    WorkerMessage, WorkerSupervisor, WorkerSupervisorError, WorkspaceId, WorkspacePath,
    materialize_artifact,
};
use tempfile::tempdir;

fn launch(mode: &str) -> WorkerLaunch {
    let mut launch = WorkerLaunch::new(PathBuf::from(env!("CARGO_BIN_EXE_kide-fixture-worker")));
    launch.args = vec![OsString::from(mode)];
    launch.idle_timeout = Duration::from_millis(10);
    launch.request_timeout = if mode == "sleep" {
        Duration::from_millis(100)
    } else {
        Duration::from_secs(2)
    };
    launch
}

#[test]
fn cold_start_handshake_batch_idle_shutdown_and_restart() {
    let mut supervisor = WorkerSupervisor::new(launch("normal"));
    let handshake = supervisor.handshake("handshake-1").expect("cold handshake");
    assert_eq!(
        handshake.capabilities.identity.backend,
        "kide-fixture-worker"
    );
    assert_eq!(supervisor.start_count(), 1);

    let response = supervisor
        .request(batch("batch-1", 2))
        .expect("multi-file batch");
    let WorkerMessage::AnalysisBatchResponse(response) = response.message else {
        panic!("batch response");
    };
    assert_eq!(response.snapshots.len(), 2);
    assert!(supervisor.is_running());

    std::thread::sleep(Duration::from_millis(15));
    assert!(supervisor.reap_idle());
    assert!(!supervisor.is_running());

    supervisor
        .handshake("handshake-2")
        .expect("restarts after idle shutdown");
    assert_eq!(supervisor.start_count(), 2);
}

#[test]
fn crash_and_timeout_leave_supervisor_recoverable() {
    let mut crashed = WorkerSupervisor::new(launch("crash"));
    assert!(matches!(
        crashed.handshake("crash-1"),
        Err(WorkerSupervisorError::Exited)
    ));
    assert!(!crashed.is_running());

    let mut timed_out = WorkerSupervisor::new(launch("sleep"));
    assert!(matches!(
        timed_out.handshake("timeout-1"),
        Err(WorkerSupervisorError::TimedOut { .. })
    ));
    assert!(!timed_out.is_running());
}

#[test]
fn duplicate_request_ids_are_rejected_without_a_second_worker_call() {
    let mut supervisor = WorkerSupervisor::new(launch("normal"));
    supervisor.handshake("handshake-1").expect("starts worker");
    supervisor
        .request(batch("batch-1", 1))
        .expect("first batch");

    assert!(matches!(
        supervisor.request(batch("batch-1", 1)),
        Err(WorkerSupervisorError::DuplicateRequestId { request_id }) if request_id == "batch-1"
    ));
    assert_eq!(supervisor.start_count(), 1);
}

#[test]
fn malformed_protobuf_frame_stops_the_worker() {
    let mut supervisor = WorkerSupervisor::new(launch("malformed"));
    assert!(matches!(
        supervisor.handshake("malformed-1"),
        Err(WorkerSupervisorError::Frame(_))
    ));
    assert!(!supervisor.is_running());
}

#[test]
fn mismatched_protobuf_request_id_stops_the_worker() {
    let mut supervisor = WorkerSupervisor::new(launch("mismatched-id"));
    assert!(matches!(
        supervisor.handshake("expected-1"),
        Err(WorkerSupervisorError::RequestIdMismatch { expected, received })
            if expected == "expected-1" && received == "unexpected-request-id"
    ));
    assert!(!supervisor.is_running());
}

#[test]
fn materialized_artifact_is_promoted_once_then_reused_from_cache() {
    let directory = tempdir().expect("temporary directories");
    let cache = ArtifactBlobCache::open(directory.path().join("cache")).expect("opens cache");
    let staging = directory.path().join("staging");
    let mut supervisor = WorkerSupervisor::new(launch("materialize"));
    supervisor
        .handshake("materialize-handshake")
        .expect("starts worker");

    assert!(
        materialize_artifact(
            &mut supervisor,
            &cache,
            WorkspacePath::new("."),
            descriptor(),
            &staging,
            "materialize-1"
        )
        .expect("promotes miss")
    );
    assert!(
        !materialize_artifact(
            &mut supervisor,
            &cache,
            WorkspacePath::new("."),
            descriptor(),
            &staging,
            "materialize-2"
        )
        .expect("reuses hit")
    );
    assert!(
        std::fs::read_dir(&staging)
            .expect("reads staging directory")
            .next()
            .is_none(),
        "successful promotion removes its staging file"
    );
}

fn batch(request_id: &str, count: usize) -> WorkerEnvelope {
    WorkerEnvelope::new(
        request_id,
        WorkerMessage::AnalyzeBatchRequest(AnalyzeBatchRequest {
            workspace: WorkspaceId::new("workspace:fixture"),
            project_fingerprint: Fingerprint::new("sha256:project"),
            requested_facts: vec![AnalysisFact::Symbols],
            source_units: (0..count).map(source_unit).collect(),
        }),
    )
}

fn source_unit(index: usize) -> SourceUnit {
    SourceUnit {
        id: SourceUnitId::new(format!("fixture:{index}")),
        component: ComponentId::new("filesystem:root:main"),
        path: WorkspacePath::new(format!("src/File{index}.kt")),
        language: Language::Kotlin,
        origin: SourceOrigin::Source,
        content: Fingerprint::new(format!("sha256:content-{index}")),
        context: Fingerprint::new("sha256:context"),
    }
}

fn descriptor() -> ArtifactDescriptor {
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
        provenance: kide_core::Provenance {
            backend: "kide-fixture-worker".into(),
            backend_version: "0.1.0".into(),
            protocol_version: kide_core::WORKER_PROTOCOL_VERSION,
            analysis_options: Fingerprint::new("sha256:fixture"),
        },
        symbol_locators: Vec::new(),
    }
}
