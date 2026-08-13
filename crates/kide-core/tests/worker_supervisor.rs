use std::{ffi::OsString, path::PathBuf, time::Duration};

use kide_core::{
    AnalysisFact, AnalyzeBatchRequest, ComponentId, Fingerprint, Language, SourceOrigin,
    SourceUnit, SourceUnitId, WorkerEnvelope, WorkerLaunch, WorkerMessage, WorkerSupervisor,
    WorkerSupervisorError, WorkspaceId, WorkspacePath,
};

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
