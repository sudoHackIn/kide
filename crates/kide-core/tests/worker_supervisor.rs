use std::{ffi::OsString, path::PathBuf, time::Duration};

use kide_core::{
    execute_semantic_capability, materialize_artifact, plan_semantic_capability, AnalysisFact,
    AnalyzeBatchRequest, ArtifactBlobCache, ArtifactDescriptor, BuildSystem, ComponentId,
    DiscoveredWorker, Fingerprint, Language, SemanticCapabilityError, SemanticQueryBudget,
    SemanticQueryResponseState, SourceOrigin, SourceUnit, SourceUnitId, WorkerEnvelope,
    WorkerInstallation, WorkerLaunch, WorkerMessage, WorkerRegistry, WorkerSupervisor,
    WorkerSupervisorError, WorkspaceId, WorkspacePath,
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
        .promoted
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
        .promoted
    );
    assert!(
        std::fs::read_dir(&staging)
            .expect("reads staging directory")
            .next()
            .is_none(),
        "successful promotion removes its staging file"
    );
}

#[test]
fn bounded_semantic_capability_succeeds_and_rejects_over_budget_worker_output() {
    let normal = discovered_worker("normal");
    let plan = plan_semantic_capability(
        &normal,
        "fixture.echo",
        1,
        Vec::new(),
        vec![
            kide_core::SymbolId::new("symbol:b"),
            kide_core::SymbolId::new("symbol:a"),
        ],
        Vec::new(),
        SemanticQueryBudget {
            max_candidates: 2,
            max_nodes: 2,
            max_bytes: 4096,
            deadline_millis: 2_000,
        },
    )
    .expect("plans advertised capability");
    let mut supervisor = WorkerSupervisor::new(launch("normal"));
    let response = execute_semantic_capability(&mut supervisor, &plan, "capability-1")
        .expect("executes bounded capability");
    assert_eq!(response.state, SemanticQueryResponseState::Complete);
    assert_eq!(
        response
            .candidate_symbols
            .iter()
            .map(|symbol| symbol.as_str())
            .collect::<Vec<_>>(),
        vec!["symbol:a", "symbol:b"]
    );
    assert_eq!(response.provenance.backend, "kide-fixture-worker");

    let over_budget = discovered_worker("capability-over-budget");
    let plan = plan_semantic_capability(
        &over_budget,
        "fixture.echo",
        1,
        Vec::new(),
        vec![kide_core::SymbolId::new("symbol:a")],
        Vec::new(),
        SemanticQueryBudget {
            max_candidates: 1,
            max_nodes: 1,
            max_bytes: 4096,
            deadline_millis: 2_000,
        },
    )
    .expect("plans bounded capability");
    let mut supervisor = WorkerSupervisor::new(launch("capability-over-budget"));
    assert!(matches!(
        execute_semantic_capability(&mut supervisor, &plan, "capability-over-budget-1"),
        Err(SemanticCapabilityError::BudgetExceeded("node"))
    ));

    let sleeping = discovered_worker("capability-sleep");
    let plan = plan_semantic_capability(
        &sleeping,
        "fixture.echo",
        1,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        SemanticQueryBudget {
            max_candidates: 1,
            max_nodes: 1,
            max_bytes: 4096,
            deadline_millis: 50,
        },
    )
    .expect("plans deadline-bounded capability");
    let mut supervisor = WorkerSupervisor::new(launch("capability-sleep"));
    assert!(matches!(
        execute_semantic_capability(&mut supervisor, &plan, "capability-sleep-1"),
        Err(SemanticCapabilityError::Supervisor(
            WorkerSupervisorError::TimedOut { timeout }
        )) if timeout == Duration::from_millis(50)
    ));
}

fn discovered_worker(mode: &str) -> DiscoveredWorker {
    let registry = WorkerRegistry::new(vec![WorkerInstallation {
        name: "fixture".to_owned(),
        launch: launch(mode),
        build_systems: vec![BuildSystem::Filesystem],
    }]);
    registry
        .discover()
        .expect("discovers fixture worker")
        .into_iter()
        .next()
        .expect("fixture worker")
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
