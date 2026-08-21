//! Deterministic framed-Protobuf worker used only by supervisor integration tests.

use std::{
    io::{self, BufReader, Write},
    thread,
    time::Duration,
};

use kide_core::{
    AnalysisBatchResponse, ArtifactAnalysisResponse, ArtifactDescriptor, ArtifactDiscoveryResponse,
    ArtifactMaterializationResponse, Completeness, FileAnalysisSnapshot, Fingerprint,
    HandshakeResponse, Language, Provenance, SemanticQueryCapability, SemanticQueryResponse,
    SemanticQueryResponseState, SemanticQueryResultKind, SourceOrigin, SourceUnit, SourceUnitId,
    WORKER_PROTOCOL_VERSION, WorkerCapabilities, WorkerCapability, WorkerEnvelope, WorkerIdentity,
    WorkerMessage, WorkspacePath, artifact_blob_layout::ArtifactBlobLayout, artifact_proto,
    worker_framing, worker_proto_adapter,
};
use sha2::{Digest, Sha256};

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
                        semantic_query_capabilities: vec![SemanticQueryCapability {
                            name: "fixture.echo".to_owned(),
                            version: 1,
                            parameters: Vec::new(),
                            result: SemanticQueryResultKind::CandidateSymbols,
                        }],
                    },
                })
            }
            WorkerMessage::AnalyzeBatchRequest(request) => {
                WorkerMessage::AnalysisBatchResponse(AnalysisBatchResponse {
                    snapshots: if mode != "missing" {
                        request
                            .source_units
                            .into_iter()
                            .map(|source_unit| FileAnalysisSnapshot {
                                source_unit,
                                structural_fingerprint: None,
                                public_api_fingerprint: None,
                                symbols: Vec::new(),
                                applications: Vec::new(),
                                occurrences: Vec::new(),
                                references: Vec::new(),
                                calls: Vec::new(),
                                hierarchy: Vec::new(),
                                types: Vec::new(),
                                diagnostics: Vec::new(),
                                completeness: Completeness::Partial,
                                provenance: provenance(),
                            })
                            .collect()
                    } else {
                        Vec::new()
                    },
                    timings: Vec::new(),
                    artifact_candidates: Vec::new(),
                    metrics: Vec::new(),
                })
            }
            WorkerMessage::ArtifactAnalysisRequest(_) => {
                WorkerMessage::ArtifactAnalysisResponse(ArtifactAnalysisResponse {
                    snapshots: Vec::new(),
                    next_cursor: None,
                })
            }
            WorkerMessage::ArtifactDiscoveryRequest(request) => {
                let (artifacts, next_cursor) = if mode == "materialize" {
                    (vec![descriptor()], None)
                } else if mode == "paginated" {
                    match request.cursor.as_deref() {
                        None => (vec![descriptor()], Some("fixture-page-1".to_owned())),
                        Some("fixture-page-1") => (vec![second_descriptor()], None),
                        Some(_) => continue,
                    }
                } else {
                    (Vec::new(), None)
                };
                WorkerMessage::ArtifactDiscoveryResponse(ArtifactDiscoveryResponse {
                    artifacts,
                    next_cursor,
                    artifact_locators: Vec::new(),
                })
            }
            WorkerMessage::ArtifactMaterializationRequest(request) if mode == "materialize" => {
                std::fs::create_dir_all(&request.staging_directory)?;
                let bytes = ArtifactBlobLayout::encode(&artifact_proto::GraphArtifact {
                    snapshots: vec![artifact_proto::GraphSnapshot {
                        source_unit: Some(artifact_proto::ArtifactSourceUnit {
                            id: "jvm:sha256:fixture-artifact".into(),
                            component: "fixture:main".into(),
                            path: ".kide/dependencies/fixture".into(),
                            language: "java".into(),
                            origin: "dependency".into(),
                            content_fingerprint: "sha256:fixture-artifact".into(),
                            context_fingerprint: "sha256:fixture-context".into(),
                        }),
                        provenances: vec![artifact_proto::ArtifactProvenance {
                            backend: "kide-fixture-worker".into(),
                            backend_version: "0.1.0".into(),
                            worker_protocol_version: WORKER_PROTOCOL_VERSION,
                            analysis_options_fingerprint: "sha256:fixture".into(),
                        }],
                        completeness: "partial".into(),
                        provenance_index: Some(0),
                        ..Default::default()
                    }],
                })
                .bytes()
                .to_vec();
                let filename = "fixture-artifact.blob";
                std::fs::write(
                    std::path::Path::new(&request.staging_directory).join(filename),
                    &bytes,
                )?;
                WorkerMessage::ArtifactMaterializationResponse(Box::new(
                    ArtifactMaterializationResponse {
                        staged_filename: filename.into(),
                        byte_length: bytes.len() as u64,
                        sha256: kide_core::Fingerprint::new(format!(
                            "sha256:{}",
                            Sha256::digest(&bytes)
                                .iter()
                                .map(|byte| format!("{byte:02x}"))
                                .collect::<String>()
                        )),
                        blob_format_version: 1,
                        timings: vec![],
                        metrics: vec![],
                    },
                ))
            }
            WorkerMessage::SemanticQueryRequest(request) => {
                if mode == "capability-sleep" {
                    thread::sleep(Duration::from_millis(250));
                }
                let visited_nodes = if mode == "capability-over-budget" {
                    request.budget.max_nodes.saturating_add(1)
                } else {
                    request.candidate_symbols.len() as u32
                };
                WorkerMessage::SemanticQueryResponse(Box::new(SemanticQueryResponse {
                    capability_name: request.capability_name,
                    capability_version: request.capability_version,
                    state: SemanticQueryResponseState::Complete,
                    candidate_symbols: request.candidate_symbols,
                    snapshots: Vec::new(),
                    provenance: provenance(),
                    visited_nodes,
                    produced_bytes: 1,
                }))
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

fn descriptor() -> ArtifactDescriptor {
    ArtifactDescriptor {
        source_unit: SourceUnit {
            id: SourceUnitId::new("jvm:sha256:fixture-artifact"),
            component: kide_core::ComponentId::new("fixture:main"),
            path: kide_core::WorkspacePath::new(".kide/dependencies/fixture"),
            language: Language::Java,
            origin: SourceOrigin::Dependency,
            content: kide_core::Fingerprint::new("sha256:fixture-artifact"),
            context: kide_core::Fingerprint::new("sha256:fixture-context"),
        },
        provenance: provenance(),
        resolved_identity: None,
        symbol_locators: Vec::new(),
    }
}

fn second_descriptor() -> ArtifactDescriptor {
    let mut descriptor = descriptor();
    descriptor.source_unit.id = SourceUnitId::new("jvm:sha256:fixture-artifact-two");
    descriptor.source_unit.path = WorkspacePath::new(".kide/dependencies/fixture-two");
    descriptor.source_unit.content = Fingerprint::new("sha256:fixture-artifact-two");
    descriptor
}

fn provenance() -> Provenance {
    Provenance {
        backend: "kide-fixture-worker".to_owned(),
        backend_version: "0.1.0".to_owned(),
        protocol_version: WORKER_PROTOCOL_VERSION,
        analysis_options: kide_core::Fingerprint::new("sha256:fixture"),
    }
}
