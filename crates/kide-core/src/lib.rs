//! Persistent, frontend-independent KIDE primitives.
//!
//! The canonical model intentionally persists graph facts, not a universal AST
//! or compiler object graph. Language and build-system workers remain
//! disposable compute processes as defined by ADR 0001.

pub mod artifact_blob_layout;
mod artifact_cache;
pub mod artifact_proto_adapter;
pub mod artifact_query;
pub mod framework_query;
pub mod project_query;
pub mod query_package;
pub mod query_resolver;
pub mod selector;
pub mod semantic_capability;
pub mod semantic_query;
pub mod text_index;
pub mod worker_proto {
    include!(concat!(env!("OUT_DIR"), "/kide.worker.v1.rs"));
}
pub mod artifact_proto {
    include!(concat!(env!("OUT_DIR"), "/kide.artifact.v1.rs"));
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
    use prost::Message;
    use tempfile::tempdir;

    use crate::{
        ArtifactBlobCache, ArtifactBlobKey, BackendKey, ByteRange, Completeness, ComponentId,
        Fingerprint, Freshness, Language, Provenance, SourceOrigin, SourceRange, SourceUnit,
        SourceUnitId, SymbolId, SymbolKind, SymbolRecord, WORKER_PROTOCOL_VERSION, WorkspacePath,
        worker_framing, worker_proto,
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
                    execution_plan: Some(worker_proto::OpaqueExecutionPlan {
                        backend: "fixture".to_owned(),
                        resolved_fingerprint: "sha256:fixture".to_owned(),
                        payload: Vec::new(),
                    }),
                },
            )),
        };
        assert_eq!(
            worker_framing::decode(&worker_framing::encode(&message)).expect("decodes frame"),
            message
        );
    }

    #[test]
    fn protobuf_readers_ignore_unknown_envelope_fields() {
        // Field 99 is intentionally absent from worker.proto. Protobuf's
        // forward-compatibility rule lets an older Core accept this envelope.
        let envelope = worker_proto::Envelope::decode(&[0x98, 0x06, 0x01][..])
            .expect("unknown field is skipped");
        assert_eq!(envelope.protocol_version, 0);
        assert!(envelope.message.is_none());
    }

    #[test]
    fn semantic_query_handshake_capability_round_trips_protobuf() {
        let envelope = crate::WorkerEnvelope::new(
            "semantic-capability-1",
            crate::WorkerMessage::HandshakeResponse(crate::HandshakeResponse {
                capabilities: crate::WorkerCapabilities {
                    identity: crate::WorkerIdentity {
                        backend: "fixture".to_owned(),
                        backend_version: "1.0.0".to_owned(),
                    },
                    protocol_version: WORKER_PROTOCOL_VERSION,
                    languages: vec![Language::Kotlin],
                    capabilities: vec![crate::WorkerCapability::Handshake],
                    semantic_query_capabilities: vec![crate::SemanticQueryCapability {
                        name: "hierarchy.direct".to_owned(),
                        version: 1,
                        parameters: vec![crate::SemanticQueryParameter {
                            name: "supertype".to_owned(),
                            ty: crate::SemanticQueryParameterType::SymbolId,
                            required: true,
                        }],
                        result: crate::SemanticQueryResultKind::CandidateSymbols,
                    }],
                },
            }),
        );

        let encoded = crate::worker_proto_adapter::envelope(&envelope).expect("encodes handshake");
        let restored =
            crate::worker_proto_adapter::decode_envelope(encoded).expect("decodes handshake");

        assert_eq!(restored, envelope);
    }

    #[test]
    fn semantic_query_request_and_response_round_trip_protobuf() {
        for fixture in [
            include_str!("../../../protocol/fixtures/semantic-query-request.json"),
            include_str!("../../../protocol/fixtures/semantic-query-response.json"),
        ] {
            let envelope: crate::WorkerEnvelope =
                serde_json::from_str(fixture).expect("semantic query fixture parses");
            let encoded =
                crate::worker_proto_adapter::envelope(&envelope).expect("encodes semantic query");
            let restored = crate::worker_proto_adapter::decode_envelope(encoded)
                .expect("decodes semantic query");
            assert_eq!(restored, envelope);
        }
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
            applied_symbols: vec![],
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
            timings: Vec::new(),
            artifact_candidates: vec![crate::ArtifactCandidate {
                locator: "/tmp/example.jar".into(),
                component: ComponentId::new("maven:app:main"),
                context: Fingerprint::new("sha256:context"),
            }],
            metrics: Vec::new(),
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
            resolved_identity: None,
            symbol_locators: Vec::new(),
        };
        let request = crate::ArtifactMaterializationRequest {
            workspace_root: WorkspacePath::new("."),
            artifact: descriptor,
            staging_directory: "staging-7".into(),
            blob_format_version: 1,
            artifact_locator: None,
        };
        let response = crate::ArtifactMaterializationResponse {
            staged_filename: "artifact.kide".into(),
            byte_length: 12,
            sha256: Fingerprint::new(format!("sha256:{}", "ab".repeat(32))),
            blob_format_version: 1,
            timings: vec![crate::PhaseTiming {
                phase: "artifact_extract".into(),
                elapsed_millis: 7,
            }],
            metrics: vec![crate::WorkerMetric {
                name: "artifact_input_bytes".into(),
                value: 42,
            }],
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
                    timings: Vec::new(),
                    metrics: Vec::new(),
                }
            )
            .is_err()
        );
    }

    #[test]
    fn graph_artifact_round_trips_canonical_snapshots_and_rejects_bad_envelopes() {
        let envelope: crate::WorkerEnvelope = serde_json::from_str(include_str!(
            "../../../protocol/fixtures/analysis-batch-response.json"
        ))
        .expect("fixture parses");
        let crate::WorkerMessage::AnalysisBatchResponse(response) = envelope.message else {
            panic!("fixture is an analysis response")
        };

        let encoded = crate::artifact_proto_adapter::encode_snapshots(&response.snapshots)
            .expect("encodes storage artifact");
        assert_eq!(
            crate::artifact_proto_adapter::decode_snapshots(&encoded).expect("decodes artifact"),
            response.snapshots
        );

        let mut incompatible = crate::artifact_proto::ArtifactEnvelope::decode(encoded.as_slice())
            .expect("envelope parses");
        incompatible.format_version += 1;
        assert!(matches!(
            crate::artifact_proto_adapter::decode_snapshots(&incompatible.encode_to_vec()),
            Err(crate::artifact_proto_adapter::ArtifactProtoError::UnsupportedVersion(_))
        ));

        let mut corrupt = crate::artifact_proto::ArtifactEnvelope::decode(encoded.as_slice())
            .expect("envelope parses");
        corrupt.payload_sha256[0] ^= 1;
        assert!(matches!(
            crate::artifact_proto_adapter::decode_snapshots(&corrupt.encode_to_vec()),
            Err(crate::artifact_proto_adapter::ArtifactProtoError::ChecksumMismatch)
        ));
    }

    #[test]
    fn jvm_blob_layout_validates_header_and_reads_dictionary_without_graph_decode() {
        let graph = crate::artifact_proto::GraphArtifact {
            snapshots: vec![crate::artifact_proto::GraphSnapshot {
                symbols: vec![
                    crate::artifact_proto::ArtifactSymbol {
                        id: "java:example.Widget".into(),
                        name: "Widget".into(),
                        ..Default::default()
                    },
                    crate::artifact_proto::ArtifactSymbol {
                        id: "java:example.WidgetImpl".into(),
                        backend_key: "example.WidgetImpl".into(),
                        backend_schema_version: 1,
                        language: "java".into(),
                        kind: "class".into(),
                        name: "WidgetImpl".into(),
                        qualified_name: Some("example.WidgetImpl".into()),
                        declaration: Some(crate::artifact_proto::ArtifactRange {
                            start: 7,
                            end: 17,
                        }),
                        name_range: Some(crate::artifact_proto::ArtifactRange {
                            start: 7,
                            end: 17,
                        }),
                        freshness: "fresh".into(),
                        completeness: "partial".into(),
                        component_id: "fixture:main".into(),
                        ..Default::default()
                    },
                ],
                hierarchy: vec![crate::artifact_proto::ArtifactHierarchy {
                    subtype_symbol_id: "java:example.WidgetImpl".into(),
                    supertype_symbol_id: "java:example.Widget".into(),
                    precision: "exact".into(),
                    provenance_index: Some(0),
                    subtype_symbol_ordinal: None,
                    supertype_symbol_ordinal: None,
                }],
                completeness: "partial".into(),
                ..Default::default()
            }],
        };
        let encoded = crate::artifact_blob_layout::ArtifactBlobLayout::encode(&graph);
        let validated =
            crate::artifact_blob_layout::ArtifactBlobLayout::validate(encoded.bytes().to_vec())
                .expect("fixed header and TOC validate");
        let dictionary = crate::artifact_proto::ArtifactSymbolDictionary::decode(
            validated
                .section(crate::artifact_proto::ArtifactBlobSectionKind::SymbolDictionary)
                .expect("dictionary range is available"),
        )
        .expect("dictionary protobuf decodes");
        assert_eq!(dictionary.entries[0].id, "java:example.Widget");
        let postings = validated.symbol_postings().expect("postings decode");
        assert_eq!(postings.entries[0].symbol_ordinal, 0);
        assert_eq!(
            validated
                .graph_facts()
                .expect("gzip graph facts decode")
                .snapshots[0]
                .hierarchy[0]
                .subtype_symbol_id,
            "java:example.WidgetImpl"
        );
        assert_eq!(
            validated
                .qualified_symbol_directory()
                .expect("directory decodes")
                .entries,
            vec![crate::artifact_proto::ArtifactQualifiedSymbolEntry {
                qualified_name: "example.WidgetImpl".into(),
                symbol_ordinal: 1,
            }]
        );

        let mut corrupt = encoded.bytes().to_vec();
        *corrupt.last_mut().expect("nonempty blob") ^= 1;
        let validated = crate::artifact_blob_layout::ArtifactBlobLayout::validate(corrupt)
            .expect("header remains valid");
        assert!(matches!(
            validated.section(crate::artifact_proto::ArtifactBlobSectionKind::SymbolDetailBlock),
            Err(crate::artifact_blob_layout::ArtifactBlobLayoutError::ChecksumMismatch)
        ));
    }

    #[test]
    fn dependency_detail_blocks_bound_the_selected_ordinal_payload() {
        let count = crate::artifact_blob_layout::SYMBOL_DETAIL_BLOCK_ENTRY_CAPACITY + 1;
        let graph = crate::artifact_proto::GraphArtifact {
            snapshots: vec![crate::artifact_proto::GraphSnapshot {
                symbols: (0..count)
                    .map(|ordinal| crate::artifact_proto::ArtifactSymbol {
                        id: format!("java:example.Symbol{ordinal}"),
                        name: format!("Symbol{ordinal}"),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }],
        };
        let layout = crate::artifact_blob_layout::ArtifactBlobLayout::validate(
            crate::artifact_blob_layout::ArtifactBlobLayout::encode(&graph)
                .bytes()
                .to_vec(),
        )
        .expect("valid layout");
        let block = layout
            .symbol_detail_block((count - 1) as u32)
            .expect("reads last ordinal block only");
        assert_eq!(block.first_symbol_ordinal, (count - 1) as u32);
        assert_eq!(block.entries.len(), 1);
    }

    #[test]
    fn cached_blob_reads_postings_by_range_without_loading_graph_facts() {
        let source = SourceUnit {
            id: SourceUnitId::new("jvm:example-widget"),
            component: ComponentId::new("fixture:main"),
            path: WorkspacePath::new(".kide/dependencies/widget.jar"),
            language: Language::Java,
            origin: SourceOrigin::Dependency,
            content: Fingerprint::new("sha256:artifact"),
            context: Fingerprint::new("sha256:context"),
        };
        let graph = crate::artifact_proto::GraphArtifact {
            snapshots: vec![crate::artifact_proto::GraphSnapshot {
                source_unit: Some(crate::artifact_proto::ArtifactSourceUnit {
                    id: "jvm:example-widget".into(),
                    component: "fixture:main".into(),
                    path: ".kide/dependencies/widget.jar".into(),
                    language: "java".into(),
                    origin: "dependency".into(),
                    content_fingerprint: "sha256:artifact".into(),
                    context_fingerprint: "sha256:context".into(),
                }),
                provenances: vec![crate::artifact_proto::ArtifactProvenance {
                    backend: "fixture".into(),
                    backend_version: "1".into(),
                    worker_protocol_version: WORKER_PROTOCOL_VERSION,
                    analysis_options_fingerprint: "sha256:options".into(),
                }],
                provenance_index: Some(0),
                symbols: vec![
                    crate::artifact_proto::ArtifactSymbol {
                        id: "java:example.Widget".into(),
                        backend_key: "example.Widget".into(),
                        backend_schema_version: 1,
                        language: "java".into(),
                        kind: "class".into(),
                        name: "Widget".into(),
                        declaration: Some(crate::artifact_proto::ArtifactRange {
                            start: 0,
                            end: 6,
                        }),
                        name_range: Some(crate::artifact_proto::ArtifactRange { start: 0, end: 6 }),
                        freshness: "fresh".into(),
                        completeness: "partial".into(),
                        component_id: "fixture:main".into(),
                        provenance_index: Some(0),
                        ..Default::default()
                    },
                    crate::artifact_proto::ArtifactSymbol {
                        id: "java:example.WidgetImpl".into(),
                        backend_key: "example.WidgetImpl".into(),
                        backend_schema_version: 1,
                        language: "java".into(),
                        kind: "class".into(),
                        name: "WidgetImpl".into(),
                        declaration: Some(crate::artifact_proto::ArtifactRange {
                            start: 7,
                            end: 17,
                        }),
                        name_range: Some(crate::artifact_proto::ArtifactRange {
                            start: 7,
                            end: 17,
                        }),
                        freshness: "fresh".into(),
                        completeness: "partial".into(),
                        component_id: "fixture:main".into(),
                        provenance_index: Some(0),
                        ..Default::default()
                    },
                ],
                hierarchy: vec![crate::artifact_proto::ArtifactHierarchy {
                    subtype_symbol_id: "java:example.WidgetImpl".into(),
                    supertype_symbol_id: "java:example.Widget".into(),
                    precision: "exact".into(),
                    provenance_index: Some(0),
                    subtype_symbol_ordinal: None,
                    supertype_symbol_ordinal: None,
                }],
                completeness: "partial".into(),
                ..Default::default()
            }],
        };
        let encoded = crate::artifact_blob_layout::ArtifactBlobLayout::encode(&graph);
        let layout =
            crate::artifact_blob_layout::ArtifactBlobLayout::validate(encoded.bytes().to_vec())
                .expect("validates detail-block layout");
        let detail = layout
            .symbol_detail_block(1)
            .expect("selected ordinal reads one block");
        assert_eq!(detail.first_symbol_ordinal, 0);
        let reconstructed = crate::artifact_proto_adapter::decode_symbol_detail(detail, 1)
            .expect("reconstructs selected canonical declaration");
        assert_eq!(reconstructed.id, SymbolId::new("java:example.WidgetImpl"));
        assert_eq!(
            reconstructed.declaration.source_unit,
            SourceUnitId::new("jvm:example-widget")
        );
        assert_eq!(reconstructed.provenance.backend, "fixture");
        assert_eq!(reconstructed.component, ComponentId::new("fixture:main"));
        let directory = tempdir().expect("temporary cache");
        let cache = ArtifactBlobCache::open(directory.path()).expect("opens cache");
        let provenance = Provenance {
            backend: "fixture".into(),
            backend_version: "1".into(),
            protocol_version: WORKER_PROTOCOL_VERSION,
            analysis_options: Fingerprint::new("sha256:options"),
        };
        let key = ArtifactBlobKey::new(
            Fingerprint::new("sha256:artifact"),
            Fingerprint::new("sha256:context"),
            &provenance,
        );
        cache
            .publish(&key, encoded.bytes())
            .expect("publishes blob");
        let mut blob = cache
            .open_blob(&key)
            .expect("opens blob")
            .expect("blob exists");
        let sections = crate::artifact_blob_layout::ArtifactBlobSections::open(&mut blob)
            .expect("reads header and toc only");
        assert_eq!(
            sections
                .symbol_postings(&mut blob)
                .expect("reads postings")
                .entries[0]
                .symbol_ordinal,
            0
        );
        let postings = sections.symbol_postings(&mut blob).expect("reads postings");
        let symbol = crate::artifact_proto_adapter::decode_symbol_detail(
            sections
                .symbol_detail_block(&mut blob, postings.entries[0].symbol_ordinal)
                .expect("reads bounded detail block"),
            postings.entries[0].symbol_ordinal,
        )
        .expect("decodes canonical symbol");
        assert_eq!(symbol.id.as_str(), "java:example.Widget");
        assert_eq!(symbol.declaration.source_unit, source.id);
        assert_eq!(
            crate::artifact_query::direct_implementations(
                &cache,
                &crate::ArtifactDescriptor {
                    source_unit: source,
                    provenance,
                    resolved_identity: None,
                    symbol_locators: Vec::new()
                },
                &crate::SymbolId::new("java:example.Widget")
            )
            .expect("reads hierarchy")
            .into_iter()
            .map(|symbol| symbol.id)
            .collect::<Vec<_>>(),
            vec![crate::SymbolId::new("java:example.WidgetImpl")]
        );
    }

    #[test]
    fn rust_reads_the_kotlin_produced_jvm_blob_golden_fixture() {
        // Generated by JvmArtifactBlobLayout.encode() for one Widget symbol.
        let bytes = decode_base64(
            "S0lERUpWTTEBAAAAHAAAAAAAAABXAAAAAAAAAAgBEigIARBzGB8iIA489LPoUAvcGllTAE+K7M0J/nOnVWSAFr91EmHoc5C4EikIAhCSARghIiCkA26Ly4pgd+PWgqAmnGisI4k48QpBszBrVFP7+rNQeQodChNqYXZhOmZpeHR1cmUuV2lkZ2V0EgZXaWRnZXQKHyodChNqYXZhOmZpeHR1cmUuV2lkZ2V0MgZXaWRnZXQ=",
        );
        let layout = crate::artifact_blob_layout::ArtifactBlobLayout::validate(bytes)
            .expect("Rust accepts Kotlin header and TOC");
        let dictionary = crate::artifact_proto::ArtifactSymbolDictionary::decode(
            layout
                .section(crate::artifact_proto::ArtifactBlobSectionKind::SymbolDictionary)
                .expect("dictionary range"),
        )
        .expect("Kotlin protobuf dictionary decodes");
        assert_eq!(dictionary.entries[0].id, "java:fixture.Widget");
    }

    fn decode_base64(value: &str) -> Vec<u8> {
        fn digit(byte: u8) -> u8 {
            match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => panic!("invalid base64"),
            }
        }
        let mut output = Vec::new();
        for chunk in value.as_bytes().chunks(4) {
            let values = [
                digit(chunk[0]),
                digit(chunk[1]),
                if chunk[2] == b'=' { 0 } else { digit(chunk[2]) },
                if chunk[3] == b'=' { 0 } else { digit(chunk[3]) },
            ];
            output.push((values[0] << 2) | (values[1] >> 4));
            if chunk[2] != b'=' {
                output.push((values[1] << 4) | (values[2] >> 2));
            }
            if chunk[3] != b'=' {
                output.push((values[2] << 6) | values[3]);
            }
        }
        output
    }
}
mod canonical;
mod config;
mod dependency_identity;
mod discovery;
mod freshness;
mod input_inventory;
mod orchestrator;
mod protocol;
mod query;
mod store;
mod supervisor;
mod worker_registry;
mod workspace;

pub use artifact_cache::*;
pub use canonical::*;
pub use config::*;
pub use dependency_identity::*;
pub use discovery::*;
pub use framework_query::*;
pub use freshness::*;
pub use input_inventory::{
    ConfigurationInput, ConfigurationInputReconciliation, ConfigurationInputState,
    ConfigurationInputStatus, SourceFileMetadata, WorkspaceCheckpoint,
    fingerprint_artifact_catalog, fingerprint_configuration_inputs, fingerprint_source_inputs,
    reconcile_configuration_inputs,
};
pub use orchestrator::*;
pub use protocol::*;
pub use query::*;
pub use semantic_capability::*;
pub use store::*;
pub use supervisor::*;
pub use text_index::*;
pub use worker_registry::*;
pub use workspace::find_workspace_root;

/// Version of the normalized records and JSON envelopes owned by KIDE Core.
pub const CANONICAL_SCHEMA_VERSION: u32 = 1;

/// Format of the physical persistent index.
///
/// The storage engine may evolve independently, but a reader must reject a
/// newer incompatible format rather than treating it as fresh data.
pub const INDEX_FORMAT_VERSION: u32 = 13;
