//! Deterministic NDJSON worker used only by supervisor integration tests.

use std::{
    io::{self, BufRead, Write},
    thread,
    time::Duration,
};

use kide_core::{
    AnalysisBatchResponse, Completeness, FileAnalysisSnapshot, HandshakeResponse, Language,
    Provenance, WORKER_PROTOCOL_VERSION, WorkerCapabilities, WorkerCapability, WorkerEnvelope,
    WorkerIdentity, WorkerMessage,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "normal".to_owned());
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let request: WorkerEnvelope = serde_json::from_str(&line?)?;
        if mode == "crash" {
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
            _ => continue,
        };
        let response = WorkerEnvelope::new(request.request_id, message);
        serde_json::to_writer(&mut stdout, &response)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
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
