//! Persistent, frontend-independent KIDE primitives.
//!
//! The canonical model intentionally persists graph facts, not a universal AST
//! or compiler object graph. Language and build-system workers remain
//! disposable compute processes as defined by ADR 0001.

mod artifact_cache;
pub mod worker_proto {
    include!(concat!(env!("OUT_DIR"), "/kide.worker.v1.rs"));
}

/// Length-delimited protobuf frames for future cold-worker transport.
pub mod worker_framing {
    use std::io::{self, Read, Write};

    use prost::Message;
    use thiserror::Error;

    use crate::worker_proto::Envelope;

    #[derive(Debug, Error)]
    pub enum FrameError {
        #[error("protobuf frame I/O failed: {0}")]
        Io(#[from] io::Error),
        #[error("invalid protobuf frame length")]
        InvalidLength,
        #[error("protobuf frame length does not match its payload")]
        LengthMismatch,
        #[error("protobuf payload failed to decode: {0}")]
        Decode(#[from] prost::DecodeError),
    }

    pub fn encode(envelope: &Envelope) -> Vec<u8> {
        let mut frame = Vec::with_capacity(envelope.encoded_len() + 10);
        let mut length = envelope.encoded_len() as u64;
        while length >= 0x80 {
            frame.push((length as u8) | 0x80);
            length >>= 7;
        }
        frame.push(length as u8);
        envelope
            .encode(&mut frame)
            .expect("Vec reserves enough space");
        frame
    }

    pub fn decode(frame: &[u8]) -> Result<Envelope, FrameError> {
        let mut length = 0_u64;
        let mut shift = 0;
        let mut offset = 0;
        for byte in frame {
            length |= u64::from(byte & 0x7f) << shift;
            offset += 1;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift >= 64 {
                return Err(FrameError::InvalidLength);
            }
        }
        let payload = frame.get(offset..).ok_or(FrameError::InvalidLength)?;
        if usize::try_from(length).ok() != Some(payload.len()) {
            return Err(FrameError::LengthMismatch);
        }
        Ok(Envelope::decode(payload)?)
    }

    /// Reads one unsigned-varint-length-delimited protobuf envelope. `None`
    /// is a clean EOF before the next frame begins.
    pub fn read_frame(reader: &mut impl Read) -> Result<Option<Envelope>, FrameError> {
        let mut first = [0_u8; 1];
        match reader.read(&mut first)? {
            0 => return Ok(None),
            1 => {}
            _ => unreachable!("single byte buffer cannot read more than one byte"),
        }

        let mut prefix = vec![first[0]];
        while prefix.last().is_some_and(|byte| byte & 0x80 != 0) {
            if prefix.len() == 10 {
                return Err(FrameError::InvalidLength);
            }
            let mut byte = [0_u8; 1];
            reader.read_exact(&mut byte)?;
            prefix.push(byte[0]);
        }

        let mut length = 0_u64;
        for (index, byte) in prefix.iter().enumerate() {
            length |= u64::from(byte & 0x7f) << (index * 7);
        }
        let length = usize::try_from(length).map_err(|_| FrameError::InvalidLength)?;
        let mut payload = vec![0_u8; length];
        reader.read_exact(&mut payload)?;
        Ok(Some(Envelope::decode(payload.as_slice())?))
    }

    pub fn write_frame(writer: &mut impl Write, envelope: &Envelope) -> Result<(), FrameError> {
        writer.write_all(&encode(envelope))?;
        writer.flush()?;
        Ok(())
    }
}

pub mod worker_proto_adapter;

#[cfg(test)]
mod worker_framing_tests {
    use crate::{
        BackendKey, ByteRange, Completeness, ComponentId, Fingerprint, Freshness, Language,
        Provenance, SourceOrigin, SourceRange, SourceUnit, SourceUnitId, SymbolId, SymbolKind,
        SymbolRecord, WorkspacePath, worker_framing, worker_proto,
    };

    #[test]
    fn protobuf_frames_round_trip_a_descriptor_request() {
        let message = worker_proto::Envelope {
            protocol_version: 3,
            request_id: "descriptor-1".to_owned(),
            message: Some(worker_proto::envelope::Message::ArtifactDiscoveryRequest(
                worker_proto::ArtifactDiscoveryRequest {
                    workspace_root: ".".to_owned(),
                    max_artifacts: 8,
                    cursor: Some("cursor-7".to_owned()),
                },
            )),
        };
        assert_eq!(
            worker_framing::decode(&worker_framing::encode(&message)).expect("decodes frame"),
            message
        );
    }

    #[test]
    fn source_unit_adapter_preserves_snapshot_identity() {
        let source = SourceUnit {
            id: SourceUnitId::new("gradle:app:Main.kt"),
            component: ComponentId::new("gradle:app:main"),
            path: WorkspacePath::new("src/Main.kt"),
            language: Language::Kotlin,
            origin: SourceOrigin::Generated,
            content: Fingerprint::new("sha256:content"),
            context: Fingerprint::new("sha256:context"),
        };

        let restored = crate::worker_proto_adapter::decode_source_unit(
            crate::worker_proto_adapter::source_unit(&source),
        )
        .expect("known source origin decodes");

        assert_eq!(restored, source);
    }

    #[test]
    fn analyze_batch_request_adapter_round_trips() {
        let envelope: crate::WorkerEnvelope = serde_json::from_str(include_str!(
            "../../../protocol/fixtures/analyze-batch-request.json"
        ))
        .expect("fixture parses");
        let crate::WorkerMessage::AnalyzeBatchRequest(request) = envelope.message else {
            panic!("fixture is an analyze request")
        };

        let restored = crate::worker_proto_adapter::decode_analyze_batch_request(
            crate::worker_proto_adapter::analyze_batch_request(&request),
        )
        .expect("request decodes");

        assert_eq!(restored, request);
    }

    #[test]
    fn analysis_delta_adapter_round_trips() {
        let envelope: crate::WorkerEnvelope = serde_json::from_str(include_str!(
            "../../../protocol/fixtures/analysis-delta.json"
        ))
        .expect("fixture parses");
        let crate::WorkerMessage::AnalysisDelta(delta) = envelope.message else {
            panic!("fixture is a delta")
        };

        let restored = crate::worker_proto_adapter::decode_analysis_delta(
            crate::worker_proto_adapter::analysis_delta(&delta).expect("delta encodes"),
        )
        .expect("delta decodes");

        assert_eq!(restored, *delta);
    }

    #[test]
    fn artifact_analysis_adapters_round_trip_and_reject_empty_page_size() {
        let request = crate::ArtifactAnalysisRequest {
            workspace_root: WorkspacePath::new("."),
            max_artifacts: 8,
            cursor: Some("page-2".into()),
        };
        let response = crate::ArtifactAnalysisResponse {
            snapshots: Vec::new(),
            next_cursor: Some("page-3".into()),
        };

        assert_eq!(
            crate::worker_proto_adapter::decode_artifact_analysis_request(
                crate::worker_proto_adapter::artifact_analysis_request(&request),
            )
            .expect("request decodes"),
            request
        );
        assert_eq!(
            crate::worker_proto_adapter::decode_artifact_analysis_response(
                crate::worker_proto_adapter::artifact_analysis_response(&response)
                    .expect("response encodes"),
            )
            .expect("response decodes"),
            response
        );
        assert!(
            crate::worker_proto_adapter::decode_artifact_analysis_request(
                worker_proto::ArtifactAnalysisRequest {
                    workspace_root: ".".into(),
                    max_artifacts: 0,
                    cursor: None,
                }
            )
            .is_err()
        );
    }

    #[test]
    fn symbol_adapter_round_trips_declaration_metadata() {
        let source = SourceUnit {
            id: SourceUnitId::new("unit"),
            component: ComponentId::new("component"),
            path: WorkspacePath::new("Main.kt"),
            language: Language::Kotlin,
            origin: SourceOrigin::Source,
            content: Fingerprint::new("sha256:content"),
            context: Fingerprint::new("sha256:context"),
        };
        let provenance = Provenance {
            backend: "kotlin".into(),
            backend_version: "1".into(),
            protocol_version: 3,
            analysis_options: Fingerprint::new("sha256:options"),
        };
        let symbol = SymbolRecord {
            id: SymbolId::new("symbol"),
            backend_key: BackendKey {
                backend: "kotlin".into(),
                schema_version: 1,
                value: "key".into(),
            },
            language: Language::Kotlin,
            kind: SymbolKind::Class,
            name: "Main".into(),
            qualified_name: Some("demo.Main".into()),
            signature: None,
            component: source.component.clone(),
            declaration: SourceRange {
                source_unit: source.id.clone(),
                bytes: ByteRange { start: 0, end: 4 },
            },
            name_range: SourceRange {
                source_unit: source.id.clone(),
                bytes: ByteRange { start: 0, end: 4 },
            },
            owner: None,
            modifiers: vec!["public".into()],
            annotations: vec![],
            freshness: Freshness::Fresh,
            completeness: Completeness::Complete,
            provenance: provenance.clone(),
        };

        let restored = crate::worker_proto_adapter::decode_symbol_declaration(
            crate::worker_proto_adapter::symbol_declaration(&symbol),
            &source,
            &provenance,
        )
        .expect("encoded declaration decodes");

        assert_eq!(restored, symbol);
    }

    #[test]
    fn analysis_snapshot_adapter_round_trips_every_fact_table() {
        let envelope: crate::WorkerEnvelope = serde_json::from_str(include_str!(
            "../../../protocol/fixtures/analysis-batch-response.json"
        ))
        .expect("fixture parses");
        let crate::WorkerMessage::AnalysisBatchResponse(response) = envelope.message else {
            panic!("fixture is an analysis response")
        };
        let mut snapshot = response
            .snapshots
            .into_iter()
            .next()
            .expect("fixture snapshot");
        let occurrence = crate::SourceOccurrence {
            range: SourceRange {
                source_unit: snapshot.source_unit.id.clone(),
                bytes: ByteRange { start: 30, end: 34 },
            },
            kind: crate::OccurrenceKind::Call,
            enclosing_symbol: snapshot.symbols.first().map(|symbol| symbol.id.clone()),
            target: Some(SymbolId::new("kotlin:demo.target")),
            type_id: Some(crate::TypeId::new("kotlin:String")),
            precision: crate::Precision::Exact,
            freshness: Freshness::Fresh,
            completeness: Completeness::Complete,
            provenance: snapshot.provenance.clone(),
        };
        snapshot.occurrences = vec![occurrence.clone()];
        snapshot.references = vec![crate::ReferenceEdge {
            source: occurrence.clone(),
            target: SymbolId::new("kotlin:demo.target"),
            precision: crate::Precision::Exact,
        }];
        snapshot.calls = vec![crate::CallEdge {
            source: occurrence,
            target: SymbolId::new("kotlin:demo.target"),
            caller: snapshot.symbols.first().map(|symbol| symbol.id.clone()),
            precision: crate::Precision::Exact,
        }];
        snapshot.hierarchy = vec![crate::HierarchyEdge {
            subtype: SymbolId::new("kotlin:demo.Child"),
            supertype: SymbolId::new("kotlin:demo.Parent"),
            precision: crate::Precision::Exact,
            provenance: snapshot.provenance.clone(),
        }];
        snapshot.types = vec![crate::TypeRecord {
            id: crate::TypeId::new("kotlin:String"),
            language: Language::Kotlin,
            display: "String".into(),
            backend_key: Some(BackendKey {
                backend: snapshot.provenance.backend.clone(),
                schema_version: 1,
                value: "kotlin.String".into(),
            }),
            freshness: Freshness::Fresh,
            completeness: Completeness::Complete,
            provenance: snapshot.provenance.clone(),
        }];
        snapshot.diagnostics = vec![crate::DiagnosticRecord {
            source_unit: snapshot.source_unit.id.clone(),
            range: Some(ByteRange { start: 40, end: 45 }),
            severity: crate::DiagnosticSeverity::Warning,
            code: Some("W1".into()),
            message: "warning".into(),
            freshness: Freshness::Fresh,
            completeness: Completeness::Complete,
            provenance: snapshot.provenance.clone(),
        }];

        let response = crate::AnalysisBatchResponse {
            snapshots: vec![snapshot.clone()],
        };
        let encoded = crate::worker_proto_adapter::analysis_batch_response(&response)
            .expect("response encodes");
        let restored = crate::worker_proto_adapter::decode_analysis_batch_response(encoded)
            .expect("response decodes");

        assert_eq!(restored, response);
    }

    #[test]
    fn materialization_and_error_adapters_round_trip_and_reject_bad_metadata() {
        let descriptor = crate::ArtifactDescriptor {
            source_unit: SourceUnit {
                id: SourceUnitId::new("jvm:artifact"),
                component: ComponentId::new("gradle:main"),
                path: WorkspacePath::new(".kide/dependencies/artifact"),
                language: Language::Java,
                origin: SourceOrigin::Dependency,
                content: Fingerprint::new("sha256:artifact"),
                context: Fingerprint::new("sha256:context"),
            },
            provenance: Provenance {
                backend: "kotlin".into(),
                backend_version: "1".into(),
                protocol_version: 3,
                analysis_options: Fingerprint::new("sha256:options"),
            },
        };
        let request = crate::ArtifactMaterializationRequest {
            workspace_root: WorkspacePath::new("."),
            artifact: descriptor,
            staging_directory: "staging-7".into(),
            blob_format_version: 1,
        };
        let response = crate::ArtifactMaterializationResponse {
            staged_filename: "artifact.kide".into(),
            byte_length: 12,
            sha256: Fingerprint::new(format!("sha256:{}", "ab".repeat(32))),
            blob_format_version: 1,
        };
        let error = crate::WorkerError {
            code: crate::WorkerErrorCode::AnalysisFailed,
            message: "staging failed".into(),
            retryable: true,
            supported_protocol_version: 3,
            received_protocol_version: Some(3),
        };

        assert_eq!(
            crate::worker_proto_adapter::decode_materialization_request(
                crate::worker_proto_adapter::materialization_request(&request),
            )
            .expect("request decodes"),
            request
        );
        assert_eq!(
            crate::worker_proto_adapter::decode_materialization_response(
                crate::worker_proto_adapter::materialization_response(&response)
                    .expect("response encodes"),
            )
            .expect("response decodes"),
            response
        );
        assert_eq!(
            crate::worker_proto_adapter::decode_worker_error(
                crate::worker_proto_adapter::worker_error(&error),
            )
            .expect("error decodes"),
            error
        );
        assert!(
            crate::worker_proto_adapter::decode_materialization_response(
                worker_proto::ArtifactMaterializationResponse {
                    staged_filename: "artifact.kide".into(),
                    byte_length: 1,
                    sha256: vec![0; 31],
                    blob_format_version: 1,
                }
            )
            .is_err()
        );
    }
}
mod canonical;
mod discovery;
mod freshness;
mod orchestrator;
mod protocol;
mod query;
mod store;
mod supervisor;

pub use artifact_cache::*;
pub use canonical::*;
pub use discovery::*;
pub use freshness::*;
pub use orchestrator::*;
pub use protocol::*;
pub use query::*;
pub use store::*;
pub use supervisor::*;

/// Version of the normalized records and JSON envelopes owned by KIDE Core.
pub const CANONICAL_SCHEMA_VERSION: u32 = 1;

/// Format of the physical persistent index.
///
/// The storage engine may evolve independently, but a reader must reject a
/// newer incompatible format rather than treating it as fresh data.
pub const INDEX_FORMAT_VERSION: u32 = 3;
