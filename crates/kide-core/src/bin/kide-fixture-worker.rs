//! Deterministic framed-Protobuf worker used only by supervisor integration tests.

use std::{
    io::{self, BufReader, Write},
    thread,
    time::Duration,
};

use kide_core::{
    AnalysisBatchResponse, ArtifactAnalysisResponse, ArtifactDiscoveryResponse, Completeness,
    FileAnalysisSnapshot, HandshakeResponse, Language, Provenance, WORKER_PROTOCOL_VERSION,
    WorkerCapabilities, WorkerCapability, WorkerEnvelope, WorkerIdentity, WorkerMessage,
    worker_framing, worker_proto_adapter,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "normal".to_owned());
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    let mut stdin = BufReader::new(stdin.lock());
    while let Some(frame) = worker_framing::read_frame(&mut stdin)? {
        let request: WorkerEnvelope = worker_proto_adapter::decode_envelope(frame)?;
        if mode == "crash" {
            return Ok(());
        }
        if mode == "malformed" {
            stdout.write_all(&[1, 0xff])?;
            stdout.flush()?;
            return Ok(());
        }
        if mode == "sleep" {
            thread::sleep(Duration::from_secs(5));
        }
        let message = match request.message {
            WorkerMessage::HandshakeRequest(_) => {
                WorkerMessage::HandshakeResponse(HandshakeResponse {
                    capabilities: WorkerCapabilities {
                        identity: WorkerIdentity {
                            backend: "kide-fixture-worker".to_owned(),
                            backend_version: "0.1.0".to_owned(),
                        },
                        protocol_version: WORKER_PROTOCOL_VERSION,
                        languages: vec![Language::Kotlin, Language::Java],
                        capabilities: vec![
                            WorkerCapability::Handshake,
                            WorkerCapability::FileAnalysisSnapshot,
                        ],
                    },
                })
            }
            WorkerMessage::AnalyzeBatchRequest(request) => {
                WorkerMessage::AnalysisBatchResponse(AnalysisBatchResponse {
                    snapshots: request
                        .source_units
                        .into_iter()
                        .map(|source_unit| FileAnalysisSnapshot {
                            source_unit,
                            structural_fingerprint: None,
                            public_api_fingerprint: None,
                            symbols: Vec::new(),
                            occurrences: Vec::new(),
                            references: Vec::new(),
                            calls: Vec::new(),
                            hierarchy: Vec::new(),
                            types: Vec::new(),
                            diagnostics: Vec::new(),
                            completeness: Completeness::Partial,
                            provenance: provenance(),
                        })
                        .collect(),
                })
            }
            WorkerMessage::ArtifactAnalysisRequest(_) => {
                WorkerMessage::ArtifactAnalysisResponse(ArtifactAnalysisResponse {
                    snapshots: Vec::new(),
                    next_cursor: None,
                })
            }
            WorkerMessage::ArtifactDiscoveryRequest(_) => {
                WorkerMessage::ArtifactDiscoveryResponse(ArtifactDiscoveryResponse {
                    artifacts: Vec::new(),
                    next_cursor: None,
                })
            }
            _ => continue,
        };
        let response = WorkerEnvelope::new(
            if mode == "mismatched-id" {
                "unexpected-request-id".to_owned()
            } else {
                request.request_id
            },
            message,
        );
        worker_framing::write_frame(&mut stdout, &worker_proto_adapter::envelope(&response)?)?;
    }
    Ok(())
}

fn provenance() -> Provenance {
    Provenance {
        backend: "kide-fixture-worker".to_owned(),
        backend_version: "0.1.0".to_owned(),
        protocol_version: WORKER_PROTOCOL_VERSION,
        analysis_options: kide_core::Fingerprint::new("sha256:fixture"),
    }
}
