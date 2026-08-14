//! On-demand lifecycle management for disposable framed-Protobuf workers.
//!
//! The supervisor owns a child only while a request needs it. The child owns
//! compiler/build state; Core owns every committed fact independently.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    io::{self, BufRead, BufReader, Read},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use thiserror::Error;

use crate::{
    HandshakeRequest, HandshakeResponse, WORKER_PROTOCOL_VERSION, WorkerEnvelope, WorkerError,
    WorkerErrorCode, WorkerMessage, worker_framing, worker_proto_adapter,
};

/// Command and lifetime limits for one disposable backend implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLaunch {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// Workspace root used to resolve worker-relative source paths.
    pub working_directory: Option<PathBuf>,
    /// Process-local worker configuration; never serialized into the protocol.
    pub environment: BTreeMap<OsString, OsString>,
    pub idle_timeout: Duration,
    pub request_timeout: Duration,
}

impl WorkerLaunch {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            working_directory: None,
            environment: BTreeMap::new(),
            idle_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Error)]
pub enum WorkerSupervisorError {
    #[error("failed to start worker {program}: {source}")]
    Start { program: PathBuf, source: io::Error },
    #[error("worker pipe framing failed: {0}")]
    Frame(#[from] worker_framing::FrameError),
    #[error("worker emitted an invalid protobuf envelope: {0}")]
    InvalidResponse(#[from] worker_proto_adapter::AdapterError),
    #[error("worker did not respond within {timeout:?}")]
    TimedOut { timeout: Duration },
    #[error("worker exited before a response was available")]
    Exited,
    #[error("worker response request ID {received:?} does not match request {expected:?}")]
    RequestIdMismatch { expected: String, received: String },
    #[error("request ID {request_id:?} has already been used by this worker process")]
    DuplicateRequestId { request_id: String },
    #[error(
        "worker response uses incompatible protocol {found}; supported protocol is {supported}"
    )]
    IncompatibleProtocol { found: u32, supported: u32 },
    #[error("worker reported {error:?}")]
    WorkerReported { error: WorkerError },
    #[error("handshake response had kind {received:?}, expected handshake_response")]
    InvalidHandshake { received: Box<WorkerMessage> },
}

/// A cold child process with a single reader thread for protocol-only stdout.
struct RunningWorker {
    child: Child,
    stdin: ChildStdin,
    responses: Receiver<Result<crate::worker_proto::Envelope, worker_framing::FrameError>>,
    request_ids: BTreeSet<String>,
    last_activity: Instant,
}

/// Sends batch-sized messages to an on-demand worker and reclaims it after
/// inactivity. Calling `reap_idle` is intentionally explicit: the daemon or
/// CLI event loop can schedule it without a hidden global background thread.
pub struct WorkerSupervisor {
    launch: WorkerLaunch,
    running: Option<RunningWorker>,
    starts: u64,
}

impl WorkerSupervisor {
    pub fn new(launch: WorkerLaunch) -> Self {
        Self {
            launch,
            running: None,
            starts: 0,
        }
    }

    pub fn start_count(&self) -> u64 {
        self.starts
    }

    pub fn is_running(&mut self) -> bool {
        self.discard_exited();
        self.running.is_some()
    }

    /// Starts the worker if needed and validates its static capabilities.
    pub fn handshake(
        &mut self,
        request_id: impl Into<String>,
    ) -> Result<HandshakeResponse, WorkerSupervisorError> {
        let response = self.request(WorkerEnvelope::new(
            request_id,
            WorkerMessage::HandshakeRequest(HandshakeRequest {
                core_version: env!("CARGO_PKG_VERSION").to_owned(),
            }),
        ))?;
        match response.message {
            WorkerMessage::HandshakeResponse(response) => {
                if response.capabilities.protocol_version != WORKER_PROTOCOL_VERSION {
                    self.stop();
                    return Err(WorkerSupervisorError::IncompatibleProtocol {
                        found: response.capabilities.protocol_version,
                        supported: WORKER_PROTOCOL_VERSION,
                    });
                }
                Ok(response)
            }
            received => Err(WorkerSupervisorError::InvalidHandshake {
                received: Box::new(received),
            }),
        }
    }

    /// Sends exactly one protocol envelope. Callers put many source units in
    /// `AnalyzeBatchRequest`; this method never fans out by symbol or file.
    pub fn request(
        &mut self,
        request: WorkerEnvelope,
    ) -> Result<WorkerEnvelope, WorkerSupervisorError> {
        request.validate_protocol_version().map_err(|error| {
            WorkerSupervisorError::IncompatibleProtocol {
                found: error.received_protocol_version.unwrap_or(0),
                supported: error.supported_protocol_version,
            }
        })?;
        self.reap_idle();
        self.ensure_running()?;

        let request_id = request.request_id.clone();
        let _span = tracing::debug_span!(target: "kide::worker", "worker_request", request_id = %request_id).entered();
        tracing::debug!(target: "kide::worker", "sending request");
        let timeout = self.launch.request_timeout;
        let response = {
            let running = self.running.as_mut().expect("worker starts before request");
            if !running.request_ids.insert(request_id.clone()) {
                return Err(WorkerSupervisorError::DuplicateRequestId { request_id });
            }
            let encoded = worker_proto_adapter::envelope(&request)?;
            worker_framing::write_frame(&mut running.stdin, &encoded)?;
            running.last_activity = Instant::now();
            running.responses.recv_timeout(timeout)
        };

        let proto_envelope = match response {
            Ok(Ok(envelope)) => envelope,
            Ok(Err(error)) => {
                self.stop();
                return Err(WorkerSupervisorError::Frame(error));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                tracing::warn!(target: "kide::worker", "worker response channel disconnected");
                self.stop();
                return Err(WorkerSupervisorError::Exited);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!(target: "kide::worker", timeout_secs = timeout.as_secs(), "worker request timed out");
                self.stop();
                return Err(WorkerSupervisorError::TimedOut { timeout });
            }
        };
        let envelope = worker_proto_adapter::decode_envelope(proto_envelope)?;
        if envelope.protocol_version != WORKER_PROTOCOL_VERSION {
            self.stop();
            return Err(WorkerSupervisorError::IncompatibleProtocol {
                found: envelope.protocol_version,
                supported: WORKER_PROTOCOL_VERSION,
            });
        }
        if envelope.request_id != request_id {
            self.stop();
            return Err(WorkerSupervisorError::RequestIdMismatch {
                expected: request_id,
                received: envelope.request_id,
            });
        }
        if let WorkerMessage::Error(error) = &envelope.message {
            return Err(WorkerSupervisorError::WorkerReported {
                error: error.clone(),
            });
        }
        if let Some(running) = self.running.as_mut() {
            running.last_activity = Instant::now();
        }
        tracing::debug!(target: "kide::worker", "received response");
        Ok(envelope)
    }

    /// Reaps a child that has been idle long enough. Returns whether one was
    /// stopped. A caller normally invokes this from its event/timer loop.
    pub fn reap_idle(&mut self) -> bool {
        let expired = self
            .running
            .as_ref()
            .is_some_and(|running| running.last_activity.elapsed() >= self.launch.idle_timeout);
        if expired {
            self.stop();
        }
        expired
    }

    /// Stops the child without modifying the durable index.
    pub fn stop(&mut self) {
        if let Some(mut running) = self.running.take() {
            let _ = running.child.kill();
            let _ = running.child.wait();
        }
    }

    fn ensure_running(&mut self) -> Result<(), WorkerSupervisorError> {
        self.discard_exited();
        if self.running.is_some() {
            return Ok(());
        }

        let mut command = Command::new(&self.launch.program);
        tracing::info!(target: "kide::worker", program = %self.launch.program.display(), "starting worker");
        command
            .args(&self.launch.args)
            .envs(&self.launch.environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // stdout is reserved for framed protobuf.  Core owns the worker's
            // stderr too, so it can merge operational messages into its own
            // tracing pipeline instead of requiring a terminal attachment.
            .stderr(Stdio::piped());
        if let Some(directory) = &self.launch.working_directory {
            command.current_dir(directory);
        }
        let mut child = command
            .spawn()
            .map_err(|source| WorkerSupervisorError::Start {
                program: self.launch.program.clone(),
                source,
            })?;
        let stdin = child.stdin.take().expect("piped stdin is present");
        let stdout = child.stdout.take().expect("piped stdout is present");
        let stderr = child.stderr.take().expect("piped stderr is present");
        let (sender, responses) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match worker_framing::read_frame(&mut reader) {
                    Ok(Some(envelope)) => {
                        if sender.send(Ok(envelope)).is_err() {
                            return;
                        }
                    }
                    Ok(None) => return,
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        return;
                    }
                }
            }
        });
        thread::spawn(move || forward_worker_stderr(stderr));
        self.running = Some(RunningWorker {
            child,
            stdin,
            responses,
            request_ids: BTreeSet::new(),
            last_activity: Instant::now(),
        });
        self.starts += 1;
        tracing::info!(target: "kide::worker", starts = self.starts, "worker started");
        Ok(())
    }

    fn discard_exited(&mut self) {
        let exited = self
            .running
            .as_mut()
            .is_some_and(|running| running.child.try_wait().ok().flatten().is_some());
        if exited {
            self.running = None;
        }
    }
}

fn forward_worker_stderr(stderr: impl Read) {
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return,
            Ok(_) => {
                let message = line.trim_end();
                if message.contains(" ERROR ") {
                    tracing::error!(target: "kide::worker", worker_log = %message, "worker stderr");
                } else if message.contains(" WARN ") {
                    tracing::warn!(target: "kide::worker", worker_log = %message, "worker stderr");
                } else if message.contains(" INFO ") {
                    tracing::info!(target: "kide::worker", worker_log = %message, "worker stderr");
                } else {
                    tracing::debug!(target: "kide::worker", worker_log = %message, "worker stderr");
                }
            }
            Err(error) => {
                tracing::warn!(target: "kide::worker", %error, "failed to read worker stderr");
                return;
            }
        }
    }
}

impl Drop for WorkerSupervisor {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Converts a worker failure into the v1 protocol error envelope, for worker
/// implementations that need a shared structured error shape.
pub fn supervisor_error_envelope(
    request_id: impl Into<String>,
    error: WorkerSupervisorError,
) -> WorkerEnvelope {
    let (code, retryable) = match error {
        WorkerSupervisorError::TimedOut { .. } | WorkerSupervisorError::Exited => {
            (WorkerErrorCode::AnalysisFailed, true)
        }
        WorkerSupervisorError::IncompatibleProtocol { .. } => {
            (WorkerErrorCode::IncompatibleProtocolVersion, false)
        }
        _ => (WorkerErrorCode::Internal, false),
    };
    WorkerEnvelope::new(
        request_id,
        WorkerMessage::Error(WorkerError {
            code,
            message: error.to_string(),
            retryable,
            supported_protocol_version: WORKER_PROTOCOL_VERSION,
            received_protocol_version: None,
        }),
    )
}
