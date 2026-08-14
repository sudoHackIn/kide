use thiserror::Error;

use crate::{
    AnalysisBatchResponse, AnalysisDelta, AnalysisFact, AnalyzeBatchRequest,
    ArtifactAnalysisRequest, ArtifactAnalysisResponse, ArtifactDescriptor,
    ArtifactDiscoveryRequest, ArtifactDiscoveryResponse, ArtifactMaterializationRequest,
    ArtifactMaterializationResponse, BackendKey, BuildSystem, CallEdge, Component, ComponentId,
    DependencyEdge, DependencyTarget, DiagnosticRecord, DiagnosticSeverity, FileAnalysisSnapshot,
    Fingerprint, HierarchyEdge, Language, OccurrenceKind, Precision, ProjectManifest, Provenance,
    ReferenceEdge, SourceOccurrence, SourceOrigin, SourceRange, SourceSet, SourceUnit,
    SourceUnitId, SymbolId, SymbolKind, SymbolRecord, Toolchain, TypeId, TypeRecord,
    WorkerCapabilities, WorkerCapability, WorkerEnvelope, WorkerError, WorkerErrorCode,
    WorkerIdentity, WorkerMessage, WorkspaceId, WorkspacePath, worker_proto,
};

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("missing required protobuf field {0}")]
    Missing(&'static str),
    #[error("unsupported protobuf enum value {0}")]
    Unsupported(String),
    #[error("invalid protobuf metadata: {0}")]
    Invalid(&'static str),
}

fn worker_capability(value: &WorkerCapability) -> &'static str {
    match value {
        WorkerCapability::Handshake => "handshake",
        WorkerCapability::ProjectManifest => "project_manifest",
        WorkerCapability::FileAnalysisSnapshot => "file_analysis_snapshot",
        WorkerCapability::DependencyAnalysis => "dependency_analysis",
        WorkerCapability::AnalysisDelta => "analysis_delta",
    }
}

fn decode_worker_capability(value: String) -> Result<WorkerCapability, AdapterError> {
    match value.as_str() {
        "handshake" => Ok(WorkerCapability::Handshake),
        "project_manifest" => Ok(WorkerCapability::ProjectManifest),
        "file_analysis_snapshot" => Ok(WorkerCapability::FileAnalysisSnapshot),
        "dependency_analysis" => Ok(WorkerCapability::DependencyAnalysis),
        "analysis_delta" => Ok(WorkerCapability::AnalysisDelta),
        other => Err(AdapterError::Unsupported(format!(
            "worker capability {other}"
        ))),
    }
}

fn handshake_response(value: &crate::HandshakeResponse) -> worker_proto::HandshakeResponse {
    worker_proto::HandshakeResponse {
        backend: value.capabilities.identity.backend.clone(),
        backend_version: value.capabilities.identity.backend_version.clone(),
        protocol_version: value.capabilities.protocol_version,
        capabilities: value
            .capabilities
            .capabilities
            .iter()
            .map(worker_capability)
            .map(str::to_owned)
            .collect(),
        languages: value
            .capabilities
            .languages
            .iter()
            .map(proto_language)
            .collect(),
    }
}

fn decode_handshake_response(
    value: worker_proto::HandshakeResponse,
) -> Result<crate::HandshakeResponse, AdapterError> {
    Ok(crate::HandshakeResponse {
        capabilities: WorkerCapabilities {
            identity: WorkerIdentity {
                backend: value.backend,
                backend_version: value.backend_version,
            },
            protocol_version: value.protocol_version,
            languages: value.languages.into_iter().map(language).collect(),
            capabilities: value
                .capabilities
                .into_iter()
                .map(decode_worker_capability)
                .collect::<Result<Vec<_>, _>>()?,
        },
    })
}

/// Converts the canonical message model to the generated protobuf envelope
/// used on a worker pipe. This is the only message-type dispatch point.
pub fn envelope(value: &WorkerEnvelope) -> Result<worker_proto::Envelope, AdapterError> {
    use worker_proto::envelope::Message;

    let message = match &value.message {
        WorkerMessage::HandshakeRequest(request) => {
            Message::HandshakeRequest(worker_proto::HandshakeRequest {
                core_version: request.core_version.clone(),
            })
        }
        WorkerMessage::HandshakeResponse(response) => {
            Message::HandshakeResponse(handshake_response(response))
        }
        WorkerMessage::ProjectManifestRequest(request) => {
            Message::ProjectManifestRequest(worker_proto::ProjectManifestRequest {
                workspace_root: request.workspace_root.as_str().to_owned(),
            })
        }
        WorkerMessage::ProjectManifestResponse(response) => {
            Message::ProjectManifestResponse(worker_proto::ProjectManifestResponse {
                manifest: Some(manifest(&response.manifest)),
            })
        }
        WorkerMessage::AnalyzeBatchRequest(request) => {
            Message::AnalyzeBatchRequest(analyze_batch_request(request))
        }
        WorkerMessage::AnalysisBatchResponse(response) => {
            Message::AnalysisBatchResponse(analysis_batch_response(response)?)
        }
        WorkerMessage::ArtifactAnalysisRequest(request) => {
            Message::ArtifactAnalysisRequest(artifact_analysis_request(request))
        }
        WorkerMessage::ArtifactAnalysisResponse(response) => {
            Message::ArtifactAnalysisResponse(artifact_analysis_response(response)?)
        }
        WorkerMessage::ArtifactDiscoveryRequest(request) => {
            Message::ArtifactDiscoveryRequest(discovery_request(request))
        }
        WorkerMessage::ArtifactDiscoveryResponse(response) => {
            Message::ArtifactDiscoveryResponse(discovery_response(response))
        }
        WorkerMessage::ArtifactMaterializationRequest(request) => {
            Message::ArtifactMaterializationRequest(materialization_request(request))
        }
        WorkerMessage::ArtifactMaterializationResponse(response) => {
            Message::ArtifactMaterializationResponse(materialization_response(response)?)
        }
        WorkerMessage::AnalysisDelta(delta) => Message::AnalysisDelta(analysis_delta(delta)?),
        WorkerMessage::Error(error) => Message::Error(worker_error(error)),
    };
    Ok(worker_proto::Envelope {
        protocol_version: value.protocol_version,
        request_id: value.request_id.clone(),
        message: Some(message),
    })
}

pub fn decode_envelope(value: worker_proto::Envelope) -> Result<WorkerEnvelope, AdapterError> {
    use worker_proto::envelope::Message;

    let message = match value
        .message
        .ok_or(AdapterError::Missing("envelope.message"))?
    {
        Message::HandshakeRequest(request) => {
            WorkerMessage::HandshakeRequest(crate::HandshakeRequest {
                core_version: request.core_version,
            })
        }
        Message::HandshakeResponse(response) => {
            WorkerMessage::HandshakeResponse(decode_handshake_response(response)?)
        }
        Message::ProjectManifestRequest(request) => {
            WorkerMessage::ProjectManifestRequest(crate::ProjectManifestRequest {
                workspace_root: WorkspacePath::new(request.workspace_root),
            })
        }
        Message::ProjectManifestResponse(response) => {
            WorkerMessage::ProjectManifestResponse(crate::ProjectManifestResponse {
                manifest: decode_manifest(
                    response
                        .manifest
                        .ok_or(AdapterError::Missing("project_manifest_response.manifest"))?,
                )?,
            })
        }
        Message::AnalyzeBatchRequest(request) => {
            WorkerMessage::AnalyzeBatchRequest(decode_analyze_batch_request(request)?)
        }
        Message::AnalysisBatchResponse(response) => {
            WorkerMessage::AnalysisBatchResponse(decode_analysis_batch_response(response)?)
        }
        Message::ArtifactAnalysisRequest(request) => {
            WorkerMessage::ArtifactAnalysisRequest(decode_artifact_analysis_request(request)?)
        }
        Message::ArtifactAnalysisResponse(response) => {
            WorkerMessage::ArtifactAnalysisResponse(decode_artifact_analysis_response(response)?)
        }
        Message::ArtifactDiscoveryRequest(request) => {
            WorkerMessage::ArtifactDiscoveryRequest(ArtifactDiscoveryRequest {
                workspace_root: WorkspacePath::new(request.workspace_root),
                max_artifacts: request.max_artifacts,
                cursor: request.cursor,
            })
        }
        Message::ArtifactDiscoveryResponse(response) => {
            WorkerMessage::ArtifactDiscoveryResponse(decode_discovery_response(response)?)
        }
        Message::ArtifactMaterializationRequest(request) => {
            WorkerMessage::ArtifactMaterializationRequest(Box::new(decode_materialization_request(
                request,
            )?))
        }
        Message::ArtifactMaterializationResponse(response) => {
            WorkerMessage::ArtifactMaterializationResponse(Box::new(
                decode_materialization_response(response)?,
            ))
        }
        Message::AnalysisDelta(delta) => {
            WorkerMessage::AnalysisDelta(Box::new(decode_analysis_delta(delta)?))
        }
        Message::Error(error) => WorkerMessage::Error(decode_worker_error(error)?),
    };
    Ok(WorkerEnvelope {
        protocol_version: value.protocol_version,
        request_id: value.request_id,
        message,
    })
}

fn language(value: String) -> Language {
    match value.as_str() {
        "kotlin" => Language::Kotlin,
        "java" => Language::Java,
        "rust" => Language::Rust,
        "typescript" => Language::TypeScript,
        other => Language::Other(other.to_owned()),
    }
}

fn build_system(value: String) -> BuildSystem {
    match value.as_str() {
        "gradle" => BuildSystem::Gradle,
        "maven" => BuildSystem::Maven,
        "cargo" => BuildSystem::Cargo,
        "npm" => BuildSystem::Npm,
        "bazel" => BuildSystem::Bazel,
        "filesystem" => BuildSystem::Filesystem,
        other => BuildSystem::Other(other.to_owned()),
    }
}

fn provenance(value: worker_proto::Provenance) -> Provenance {
    Provenance {
        backend: value.backend,
        backend_version: value.backend_version,
        protocol_version: value.protocol_version,
        analysis_options: Fingerprint::new(value.analysis_options_fingerprint),
    }
}

fn proto_provenance(value: &Provenance) -> worker_proto::Provenance {
    worker_proto::Provenance {
        backend: value.backend.clone(),
        backend_version: value.backend_version.clone(),
        protocol_version: value.protocol_version,
        analysis_options_fingerprint: value.analysis_options.as_str().to_owned(),
    }
}

fn source_origin(value: String) -> Result<SourceOrigin, AdapterError> {
    match value.as_str() {
        "source" => Ok(SourceOrigin::Source),
        "generated" => Ok(SourceOrigin::Generated),
        "dependency" => Ok(SourceOrigin::Dependency),
        other => Err(AdapterError::Unsupported(format!("source origin {other}"))),
    }
}

fn proto_language(value: &Language) -> String {
    match value {
        Language::Kotlin => "kotlin".to_owned(),
        Language::Java => "java".to_owned(),
        Language::Rust => "rust".to_owned(),
        Language::TypeScript => "typescript".to_owned(),
        Language::Other(value) => value.clone(),
    }
}

fn proto_source_origin(value: &SourceOrigin) -> &'static str {
    match value {
        SourceOrigin::Source => "source",
        SourceOrigin::Generated => "generated",
        SourceOrigin::Dependency => "dependency",
    }
}

pub fn source_unit(value: &SourceUnit) -> worker_proto::SourceUnit {
    worker_proto::SourceUnit {
        id: value.id.as_str().to_owned(),
        component: value.component.as_str().to_owned(),
        path: value.path.as_str().to_owned(),
        language: proto_language(&value.language),
        origin: proto_source_origin(&value.origin).to_owned(),
        content: value.content.as_str().to_owned(),
        context: value.context.as_str().to_owned(),
    }
}

pub fn decode_source_unit(value: worker_proto::SourceUnit) -> Result<SourceUnit, AdapterError> {
    Ok(SourceUnit {
        id: SourceUnitId::new(value.id),
        component: ComponentId::new(value.component),
        path: WorkspacePath::new(value.path),
        language: language(value.language),
        origin: source_origin(value.origin)?,
        content: Fingerprint::new(value.content),
        context: Fingerprint::new(value.context),
    })
}

pub fn analyze_batch_request(value: &AnalyzeBatchRequest) -> worker_proto::AnalyzeBatchRequest {
    worker_proto::AnalyzeBatchRequest {
        workspace: value.workspace.as_str().to_owned(),
        project_fingerprint: value.project_fingerprint.as_str().to_owned(),
        requested_facts: value
            .requested_facts
            .iter()
            .map(|fact| format!("{fact:?}").to_lowercase())
            .collect(),
        source_units: value.source_units.iter().map(source_unit).collect(),
    }
}

fn analysis_fact(value: String) -> Result<AnalysisFact, AdapterError> {
    match value.as_str() {
        "symbols" => Ok(AnalysisFact::Symbols),
        "occurrences" => Ok(AnalysisFact::Occurrences),
        "references" => Ok(AnalysisFact::References),
        "calls" => Ok(AnalysisFact::Calls),
        "hierarchy" => Ok(AnalysisFact::Hierarchy),
        "types" => Ok(AnalysisFact::Types),
        other => Err(AdapterError::Unsupported(format!("analysis fact {other}"))),
    }
}

pub fn decode_analyze_batch_request(
    value: worker_proto::AnalyzeBatchRequest,
) -> Result<AnalyzeBatchRequest, AdapterError> {
    Ok(AnalyzeBatchRequest {
        workspace: WorkspaceId::new(value.workspace),
        project_fingerprint: Fingerprint::new(value.project_fingerprint),
        requested_facts: value
            .requested_facts
            .into_iter()
            .map(analysis_fact)
            .collect::<Result<Vec<_>, _>>()?,
        source_units: value
            .source_units
            .into_iter()
            .map(decode_source_unit)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

/// Encode a declaration relative to its containing `FileAnalysisSnapshot`.
/// Source and provenance indexes deliberately stay local to that snapshot.
pub fn symbol_declaration(value: &SymbolRecord) -> worker_proto::SymbolDeclaration {
    worker_proto::SymbolDeclaration {
        id: value.id.as_str().to_owned(),
        source_unit_index: 0,
        provenance_index: 0,
        backend_key: value.backend_key.value.clone(),
        backend_schema_version: value.backend_key.schema_version,
        language: proto_language(&value.language),
        kind: format!("{:?}", value.kind).to_lowercase(),
        name: value.name.clone(),
        qualified_name: value.qualified_name.clone(),
        signature: value.signature.clone(),
        declaration: Some(worker_proto::ByteRange {
            start: value.declaration.bytes.start,
            end: value.declaration.bytes.end,
        }),
        name_range: Some(worker_proto::ByteRange {
            start: value.name_range.bytes.start,
            end: value.name_range.bytes.end,
        }),
        owner_id: value.owner.as_ref().map(|owner| owner.as_str().to_owned()),
        modifiers: value.modifiers.clone(),
        annotations: value.annotations.clone(),
        freshness: format!("{:?}", value.freshness).to_lowercase(),
        completeness: format!("{:?}", value.completeness).to_lowercase(),
        component_id: value.component.as_str().to_owned(),
    }
}

fn symbol_kind(value: String) -> Result<SymbolKind, AdapterError> {
    match value.as_str() {
        "package" => Ok(SymbolKind::Package),
        "module" => Ok(SymbolKind::Module),
        "class" => Ok(SymbolKind::Class),
        "interface" => Ok(SymbolKind::Interface),
        "object" => Ok(SymbolKind::Object),
        "enum" => Ok(SymbolKind::Enum),
        "function" => Ok(SymbolKind::Function),
        "method" => Ok(SymbolKind::Method),
        "constructor" => Ok(SymbolKind::Constructor),
        "property" => Ok(SymbolKind::Property),
        "field" => Ok(SymbolKind::Field),
        "parameter" => Ok(SymbolKind::Parameter),
        "typealias" => Ok(SymbolKind::TypeAlias),
        "other" => Ok(SymbolKind::Other),
        other => Err(AdapterError::Unsupported(format!("symbol kind {other}"))),
    }
}

fn freshness(value: String) -> Result<crate::Freshness, AdapterError> {
    match value.as_str() {
        "fresh" => Ok(crate::Freshness::Fresh),
        "stale" => Ok(crate::Freshness::Stale),
        "unknown" => Ok(crate::Freshness::Unknown),
        "unsupported" => Ok(crate::Freshness::Unsupported),
        other => Err(AdapterError::Unsupported(format!("freshness {other}"))),
    }
}

fn completeness(value: String) -> Result<crate::Completeness, AdapterError> {
    match value.as_str() {
        "complete" => Ok(crate::Completeness::Complete),
        "partial" => Ok(crate::Completeness::Partial),
        "failed" => Ok(crate::Completeness::Failed),
        other => Err(AdapterError::Unsupported(format!("completeness {other}"))),
    }
}

pub fn decode_symbol_declaration(
    value: worker_proto::SymbolDeclaration,
    source_unit: &SourceUnit,
    provenance: &Provenance,
) -> Result<SymbolRecord, AdapterError> {
    let declaration = value
        .declaration
        .ok_or(AdapterError::Missing("symbol.declaration"))?;
    let name_range = value
        .name_range
        .ok_or(AdapterError::Missing("symbol.name_range"))?;
    Ok(SymbolRecord {
        id: SymbolId::new(value.id),
        backend_key: crate::BackendKey {
            backend: provenance.backend.clone(),
            schema_version: value.backend_schema_version,
            value: value.backend_key,
        },
        language: language(value.language),
        kind: symbol_kind(value.kind)?,
        name: value.name,
        qualified_name: value.qualified_name,
        signature: value.signature,
        component: ComponentId::new(value.component_id),
        declaration: SourceRange {
            source_unit: source_unit.id.clone(),
            bytes: crate::ByteRange {
                start: declaration.start,
                end: declaration.end,
            },
        },
        name_range: SourceRange {
            source_unit: source_unit.id.clone(),
            bytes: crate::ByteRange {
                start: name_range.start,
                end: name_range.end,
            },
        },
        owner: value.owner_id.map(SymbolId::new),
        modifiers: value.modifiers,
        annotations: value.annotations,
        freshness: freshness(value.freshness)?,
        completeness: completeness(value.completeness)?,
        provenance: provenance.clone(),
    })
}

fn precision(value: String) -> Result<Precision, AdapterError> {
    match value.as_str() {
        "exact" => Ok(Precision::Exact),
        "approximate" => Ok(Precision::Approximate),
        other => Err(AdapterError::Unsupported(format!("precision {other}"))),
    }
}

fn occurrence_kind(value: String) -> Result<OccurrenceKind, AdapterError> {
    match value.as_str() {
        "declaration" => Ok(OccurrenceKind::Declaration),
        "reference" => Ok(OccurrenceKind::Reference),
        "call" => Ok(OccurrenceKind::Call),
        "type_reference" => Ok(OccurrenceKind::TypeReference),
        "import" => Ok(OccurrenceKind::Import),
        other => Err(AdapterError::Unsupported(format!(
            "occurrence kind {other}"
        ))),
    }
}

pub fn occurrence(value: &SourceOccurrence) -> worker_proto::Occurrence {
    worker_proto::Occurrence {
        location: Some(worker_proto::SourceLocation {
            source_unit_index: 0,
            range: Some(worker_proto::ByteRange {
                start: value.range.bytes.start,
                end: value.range.bytes.end,
            }),
        }),
        kind: format!("{:?}", value.kind).to_lowercase(),
        enclosing_symbol_id: value
            .enclosing_symbol
            .as_ref()
            .map(|id| id.as_str().to_owned()),
        target_symbol_id: value.target.as_ref().map(|id| id.as_str().to_owned()),
        type_id: value.type_id.as_ref().map(|id| id.as_str().to_owned()),
        precision: format!("{:?}", value.precision).to_lowercase(),
        freshness: format!("{:?}", value.freshness).to_lowercase(),
        completeness: format!("{:?}", value.completeness).to_lowercase(),
        provenance_index: 0,
    }
}

pub fn decode_occurrence(
    value: worker_proto::Occurrence,
    source_unit: &SourceUnit,
    provenance: &Provenance,
) -> Result<SourceOccurrence, AdapterError> {
    let location = value
        .location
        .ok_or(AdapterError::Missing("occurrence.location"))?;
    let range = location
        .range
        .ok_or(AdapterError::Missing("occurrence.location.range"))?;
    if location.source_unit_index != 0 {
        return Err(AdapterError::Unsupported(
            "cross-snapshot occurrence source".into(),
        ));
    }
    Ok(SourceOccurrence {
        range: SourceRange {
            source_unit: source_unit.id.clone(),
            bytes: crate::ByteRange {
                start: range.start,
                end: range.end,
            },
        },
        kind: occurrence_kind(value.kind)?,
        enclosing_symbol: value.enclosing_symbol_id.map(SymbolId::new),
        target: value.target_symbol_id.map(SymbolId::new),
        type_id: value.type_id.map(TypeId::new),
        precision: precision(value.precision)?,
        freshness: freshness(value.freshness)?,
        completeness: completeness(value.completeness)?,
        provenance: provenance.clone(),
    })
}

pub fn reference_edge(
    value: &ReferenceEdge,
    source_occurrence_index: u32,
) -> worker_proto::ReferenceEdge {
    worker_proto::ReferenceEdge {
        source_occurrence_index,
        target_symbol_id: value.target.as_str().to_owned(),
        precision: format!("{:?}", value.precision).to_lowercase(),
    }
}

pub fn call_edge(value: &CallEdge, source_occurrence_index: u32) -> worker_proto::CallEdge {
    worker_proto::CallEdge {
        source_occurrence_index,
        target_symbol_id: value.target.as_str().to_owned(),
        caller_symbol_id: value.caller.as_ref().map(|id| id.as_str().to_owned()),
        precision: format!("{:?}", value.precision).to_lowercase(),
    }
}

pub fn hierarchy_edge(value: &HierarchyEdge) -> worker_proto::HierarchyEdge {
    worker_proto::HierarchyEdge {
        subtype_symbol_id: value.subtype.as_str().to_owned(),
        supertype_symbol_id: value.supertype.as_str().to_owned(),
        precision: format!("{:?}", value.precision).to_lowercase(),
        provenance_index: 0,
    }
}

pub fn diagnostic(value: &DiagnosticRecord) -> worker_proto::Diagnostic {
    worker_proto::Diagnostic {
        source_unit_index: 0,
        range: value.range.map(|range| worker_proto::ByteRange {
            start: range.start,
            end: range.end,
        }),
        severity: format!("{:?}", value.severity).to_lowercase(),
        code: value.code.clone(),
        message: value.message.clone(),
        freshness: format!("{:?}", value.freshness).to_lowercase(),
        completeness: format!("{:?}", value.completeness).to_lowercase(),
        provenance_index: 0,
    }
}

fn severity(value: String) -> Result<DiagnosticSeverity, AdapterError> {
    match value.as_str() {
        "error" => Ok(DiagnosticSeverity::Error),
        "warning" => Ok(DiagnosticSeverity::Warning),
        "information" => Ok(DiagnosticSeverity::Information),
        "hint" => Ok(DiagnosticSeverity::Hint),
        other => Err(AdapterError::Unsupported(format!(
            "diagnostic severity {other}"
        ))),
    }
}

pub fn decode_diagnostic(
    value: worker_proto::Diagnostic,
    source_unit: &SourceUnit,
    provenance: &Provenance,
) -> Result<DiagnosticRecord, AdapterError> {
    if value.source_unit_index != 0 {
        return Err(AdapterError::Unsupported(
            "cross-snapshot diagnostic source".into(),
        ));
    }
    Ok(DiagnosticRecord {
        source_unit: source_unit.id.clone(),
        range: value.range.map(|range| crate::ByteRange {
            start: range.start,
            end: range.end,
        }),
        severity: severity(value.severity)?,
        code: value.code,
        message: value.message,
        freshness: freshness(value.freshness)?,
        completeness: completeness(value.completeness)?,
        provenance: provenance.clone(),
    })
}

pub fn type_record(value: &TypeRecord) -> worker_proto::TypeRecord {
    worker_proto::TypeRecord {
        id: value.id.as_str().to_owned(),
        language: proto_language(&value.language),
        display: value.display.clone(),
        backend_key: value.backend_key.as_ref().map(|key| key.value.clone()),
        backend_schema_version: value
            .backend_key
            .as_ref()
            .map_or(0, |key| key.schema_version),
        freshness: format!("{:?}", value.freshness).to_lowercase(),
        completeness: format!("{:?}", value.completeness).to_lowercase(),
        provenance_index: 0,
    }
}

pub fn decode_type_record(
    value: worker_proto::TypeRecord,
    provenance: &Provenance,
) -> Result<TypeRecord, AdapterError> {
    Ok(TypeRecord {
        id: TypeId::new(value.id),
        language: language(value.language),
        display: value.display,
        backend_key: value.backend_key.map(|key| BackendKey {
            backend: provenance.backend.clone(),
            schema_version: value.backend_schema_version,
            value: key,
        }),
        freshness: freshness(value.freshness)?,
        completeness: completeness(value.completeness)?,
        provenance: provenance.clone(),
    })
}

pub fn decode_reference_edge(
    value: worker_proto::ReferenceEdge,
    occurrences: &[SourceOccurrence],
) -> Result<ReferenceEdge, AdapterError> {
    let source = occurrences
        .get(value.source_occurrence_index as usize)
        .cloned()
        .ok_or(AdapterError::Missing("reference.source_occurrence"))?;
    Ok(ReferenceEdge {
        source,
        target: SymbolId::new(value.target_symbol_id),
        precision: precision(value.precision)?,
    })
}

pub fn decode_call_edge(
    value: worker_proto::CallEdge,
    occurrences: &[SourceOccurrence],
) -> Result<CallEdge, AdapterError> {
    let source = occurrences
        .get(value.source_occurrence_index as usize)
        .cloned()
        .ok_or(AdapterError::Missing("call.source_occurrence"))?;
    Ok(CallEdge {
        source,
        target: SymbolId::new(value.target_symbol_id),
        caller: value.caller_symbol_id.map(SymbolId::new),
        precision: precision(value.precision)?,
    })
}

pub fn decode_hierarchy_edge(
    value: worker_proto::HierarchyEdge,
    provenance: &Provenance,
) -> Result<HierarchyEdge, AdapterError> {
    Ok(HierarchyEdge {
        subtype: SymbolId::new(value.subtype_symbol_id),
        supertype: SymbolId::new(value.supertype_symbol_id),
        precision: precision(value.precision)?,
        provenance: provenance.clone(),
    })
}

fn indexed_provenance<'a>(
    values: &'a [Provenance],
    index: u32,
    field: &'static str,
) -> Result<&'a Provenance, AdapterError> {
    values
        .get(index as usize)
        .ok_or(AdapterError::Missing(field))
}

fn provenance_index(values: &mut Vec<Provenance>, value: &Provenance) -> u32 {
    if let Some(index) = values.iter().position(|candidate| candidate == value) {
        return index as u32;
    }
    values.push(value.clone());
    (values.len() - 1) as u32
}

pub fn file_analysis_snapshot(
    value: &FileAnalysisSnapshot,
) -> Result<worker_proto::FileAnalysisSnapshot, AdapterError> {
    let mut provenances = vec![value.provenance.clone()];
    let snapshot_provenance_index = 0;
    let mut symbols = value
        .symbols
        .iter()
        .map(symbol_declaration)
        .collect::<Vec<_>>();
    for (proto, canonical) in symbols.iter_mut().zip(&value.symbols) {
        proto.provenance_index = provenance_index(&mut provenances, &canonical.provenance);
    }
    let mut occurrences = value.occurrences.iter().map(occurrence).collect::<Vec<_>>();
    for (proto, canonical) in occurrences.iter_mut().zip(&value.occurrences) {
        proto.provenance_index = provenance_index(&mut provenances, &canonical.provenance);
    }
    let occurrence_index = |occurrence: &SourceOccurrence| {
        value
            .occurrences
            .iter()
            .position(|candidate| candidate == occurrence)
            .map(|index| index as u32)
            .ok_or(AdapterError::Missing("relation.source_occurrence"))
    };
    let references = value
        .references
        .iter()
        .map(|edge| Ok(reference_edge(edge, occurrence_index(&edge.source)?)))
        .collect::<Result<Vec<_>, _>>()?;
    let calls = value
        .calls
        .iter()
        .map(|edge| Ok(call_edge(edge, occurrence_index(&edge.source)?)))
        .collect::<Result<Vec<_>, _>>()?;
    let mut hierarchy = value
        .hierarchy
        .iter()
        .map(hierarchy_edge)
        .collect::<Vec<_>>();
    for (proto, canonical) in hierarchy.iter_mut().zip(&value.hierarchy) {
        proto.provenance_index = provenance_index(&mut provenances, &canonical.provenance);
    }
    let mut types = value.types.iter().map(type_record).collect::<Vec<_>>();
    for (proto, canonical) in types.iter_mut().zip(&value.types) {
        proto.provenance_index = provenance_index(&mut provenances, &canonical.provenance);
    }
    let mut diagnostics = value.diagnostics.iter().map(diagnostic).collect::<Vec<_>>();
    for (proto, canonical) in diagnostics.iter_mut().zip(&value.diagnostics) {
        proto.provenance_index = provenance_index(&mut provenances, &canonical.provenance);
    }
    Ok(worker_proto::FileAnalysisSnapshot {
        source_unit: Some(source_unit(&value.source_unit)),
        structural_fingerprint: value
            .structural_fingerprint
            .as_ref()
            .map(|value| value.as_str().to_owned()),
        public_api_fingerprint: value
            .public_api_fingerprint
            .as_ref()
            .map(|value| value.as_str().to_owned()),
        provenances: provenances.iter().map(proto_provenance).collect(),
        symbols,
        occurrences,
        references,
        calls,
        hierarchy,
        types,
        diagnostics,
        completeness: format!("{:?}", value.completeness).to_lowercase(),
        provenance_index: snapshot_provenance_index,
    })
}

pub fn decode_file_analysis_snapshot(
    value: worker_proto::FileAnalysisSnapshot,
) -> Result<FileAnalysisSnapshot, AdapterError> {
    let source_unit = decode_source_unit(
        value
            .source_unit
            .ok_or(AdapterError::Missing("snapshot.source_unit"))?,
    )?;
    let provenances = value
        .provenances
        .into_iter()
        .map(provenance)
        .collect::<Vec<_>>();
    let snapshot_provenance = indexed_provenance(
        &provenances,
        value.provenance_index,
        "snapshot.provenance_index",
    )?
    .clone();
    let symbols = value
        .symbols
        .into_iter()
        .map(|symbol| {
            let p = indexed_provenance(
                &provenances,
                symbol.provenance_index,
                "symbol.provenance_index",
            )?;
            decode_symbol_declaration(symbol, &source_unit, p)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let occurrences = value
        .occurrences
        .into_iter()
        .map(|occurrence| {
            let p = indexed_provenance(
                &provenances,
                occurrence.provenance_index,
                "occurrence.provenance_index",
            )?;
            decode_occurrence(occurrence, &source_unit, p)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let references = value
        .references
        .into_iter()
        .map(|edge| decode_reference_edge(edge, &occurrences))
        .collect::<Result<Vec<_>, _>>()?;
    let calls = value
        .calls
        .into_iter()
        .map(|edge| decode_call_edge(edge, &occurrences))
        .collect::<Result<Vec<_>, _>>()?;
    let hierarchy = value
        .hierarchy
        .into_iter()
        .map(|edge| {
            let p = indexed_provenance(
                &provenances,
                edge.provenance_index,
                "hierarchy.provenance_index",
            )?;
            decode_hierarchy_edge(edge, p)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let types = value
        .types
        .into_iter()
        .map(|record| {
            let p = indexed_provenance(
                &provenances,
                record.provenance_index,
                "type.provenance_index",
            )?;
            decode_type_record(record, p)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let diagnostics = value
        .diagnostics
        .into_iter()
        .map(|diagnostic| {
            let p = indexed_provenance(
                &provenances,
                diagnostic.provenance_index,
                "diagnostic.provenance_index",
            )?;
            decode_diagnostic(diagnostic, &source_unit, p)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(FileAnalysisSnapshot {
        source_unit,
        structural_fingerprint: value.structural_fingerprint.map(Fingerprint::new),
        public_api_fingerprint: value.public_api_fingerprint.map(Fingerprint::new),
        symbols,
        occurrences,
        references,
        calls,
        hierarchy,
        types,
        diagnostics,
        completeness: completeness(value.completeness)?,
        provenance: snapshot_provenance,
    })
}

pub fn analysis_batch_response(
    value: &AnalysisBatchResponse,
) -> Result<worker_proto::AnalysisBatchResponse, AdapterError> {
    Ok(worker_proto::AnalysisBatchResponse {
        snapshots: value
            .snapshots
            .iter()
            .map(file_analysis_snapshot)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

pub fn decode_analysis_batch_response(
    value: worker_proto::AnalysisBatchResponse,
) -> Result<AnalysisBatchResponse, AdapterError> {
    Ok(AnalysisBatchResponse {
        snapshots: value
            .snapshots
            .into_iter()
            .map(decode_file_analysis_snapshot)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

pub fn analysis_delta(value: &AnalysisDelta) -> Result<worker_proto::AnalysisDelta, AdapterError> {
    Ok(worker_proto::AnalysisDelta {
        source_unit_id: value.source_unit.as_str().to_owned(),
        previous_content_fingerprint: value
            .previous_content
            .as_ref()
            .map(|fingerprint| fingerprint.as_str().to_owned()),
        snapshot: value
            .snapshot
            .as_ref()
            .map(file_analysis_snapshot)
            .transpose()?,
    })
}

pub fn decode_analysis_delta(
    value: worker_proto::AnalysisDelta,
) -> Result<AnalysisDelta, AdapterError> {
    Ok(AnalysisDelta {
        source_unit: SourceUnitId::new(value.source_unit_id),
        previous_content: value.previous_content_fingerprint.map(Fingerprint::new),
        snapshot: value
            .snapshot
            .map(decode_file_analysis_snapshot)
            .transpose()?,
    })
}

pub fn artifact_analysis_request(
    value: &ArtifactAnalysisRequest,
) -> worker_proto::ArtifactAnalysisRequest {
    worker_proto::ArtifactAnalysisRequest {
        workspace_root: value.workspace_root.as_str().to_owned(),
        max_artifacts: value.max_artifacts,
        cursor: value.cursor.clone(),
    }
}

pub fn decode_artifact_analysis_request(
    value: worker_proto::ArtifactAnalysisRequest,
) -> Result<ArtifactAnalysisRequest, AdapterError> {
    if value.max_artifacts == 0 {
        return Err(AdapterError::Invalid("artifact analysis max artifacts"));
    }
    Ok(ArtifactAnalysisRequest {
        workspace_root: WorkspacePath::new(value.workspace_root),
        max_artifacts: value.max_artifacts,
        cursor: value.cursor,
    })
}

pub fn artifact_analysis_response(
    value: &ArtifactAnalysisResponse,
) -> Result<worker_proto::ArtifactAnalysisResponse, AdapterError> {
    Ok(worker_proto::ArtifactAnalysisResponse {
        snapshots: value
            .snapshots
            .iter()
            .map(file_analysis_snapshot)
            .collect::<Result<Vec<_>, _>>()?,
        next_cursor: value.next_cursor.clone(),
    })
}

pub fn decode_artifact_analysis_response(
    value: worker_proto::ArtifactAnalysisResponse,
) -> Result<ArtifactAnalysisResponse, AdapterError> {
    Ok(ArtifactAnalysisResponse {
        snapshots: value
            .snapshots
            .into_iter()
            .map(decode_file_analysis_snapshot)
            .collect::<Result<Vec<_>, _>>()?,
        next_cursor: value.next_cursor,
    })
}

pub fn decode_manifest(
    value: worker_proto::ProjectManifest,
) -> Result<ProjectManifest, AdapterError> {
    let worker_provenance = value
        .provenance
        .ok_or(AdapterError::Missing("manifest.provenance"))?;
    let components = value
        .components
        .into_iter()
        .map(|component| Component {
            id: ComponentId::new(component.id),
            name: component.name,
            build_system: build_system(component.build_system),
            root: WorkspacePath::new(component.root),
            languages: component.languages.into_iter().map(language).collect(),
            configuration: Fingerprint::new(component.configuration_fingerprint),
            source_sets: component
                .source_sets
                .into_iter()
                .map(|set| SourceSet {
                    name: set.name,
                    source_roots: set
                        .source_roots
                        .into_iter()
                        .map(WorkspacePath::new)
                        .collect(),
                    generated_roots: set
                        .generated_roots
                        .into_iter()
                        .map(WorkspacePath::new)
                        .collect(),
                    test: set.test,
                })
                .collect(),
            classpath: component
                .classpath_fingerprints
                .into_iter()
                .map(Fingerprint::new)
                .collect(),
            toolchain: component.toolchain.map(|toolchain| Toolchain {
                jvm_version: toolchain.jvm_version,
                gradle_version: toolchain.gradle_version,
                kotlin_version: toolchain.kotlin_version,
            }),
            compiler_configuration: component
                .compiler_configuration_fingerprint
                .map(Fingerprint::new),
        })
        .collect();
    let dependencies = value
        .dependencies
        .into_iter()
        .map(|edge| {
            let target = edge
                .target
                .ok_or(AdapterError::Missing("dependency.target"))?;
            Ok(DependencyEdge {
                from: ComponentId::new(edge.from_component_id),
                target: match target {
                    worker_proto::dependency_edge::Target::ComponentId(component) => {
                        DependencyTarget::Component {
                            component: ComponentId::new(component),
                        }
                    }
                    worker_proto::dependency_edge::Target::ArtifactFingerprint(content) => {
                        DependencyTarget::Artifact {
                            content: Fingerprint::new(content),
                        }
                    }
                },
                scope: edge.scope,
            })
        })
        .collect::<Result<Vec<_>, AdapterError>>()?;
    Ok(ProjectManifest {
        workspace: WorkspaceId::new(value.workspace),
        root: WorkspacePath::new(value.root),
        components,
        dependencies,
        fingerprint: Fingerprint::new(value.fingerprint),
        provenance: provenance(worker_provenance),
    })
}

pub fn discovery_request(
    request: &ArtifactDiscoveryRequest,
) -> worker_proto::ArtifactDiscoveryRequest {
    worker_proto::ArtifactDiscoveryRequest {
        workspace_root: request.workspace_root.as_str().to_owned(),
        max_artifacts: request.max_artifacts,
        cursor: request.cursor.clone(),
    }
}

pub fn discovery_response(
    response: &ArtifactDiscoveryResponse,
) -> worker_proto::ArtifactDiscoveryResponse {
    worker_proto::ArtifactDiscoveryResponse {
        artifacts: response.artifacts.iter().map(descriptor).collect(),
        next_cursor: response.next_cursor.clone(),
    }
}

fn descriptor(value: &ArtifactDescriptor) -> worker_proto::ArtifactDescriptor {
    let unit = &value.source_unit;
    worker_proto::ArtifactDescriptor {
        source_unit_id: unit.id.as_str().to_owned(),
        component_id: unit.component.as_str().to_owned(),
        workspace_path: unit.path.as_str().to_owned(),
        content_fingerprint: unit.content.as_str().to_owned(),
        context_fingerprint: unit.context.as_str().to_owned(),
        backend: value.provenance.backend.clone(),
        backend_version: value.provenance.backend_version.clone(),
        worker_protocol_version: value.provenance.protocol_version,
        analysis_options_fingerprint: value.provenance.analysis_options.as_str().to_owned(),
        language: proto_language(&unit.language),
        origin: proto_source_origin(&unit.origin).to_owned(),
    }
}

pub fn decode_descriptor(
    value: worker_proto::ArtifactDescriptor,
) -> Result<ArtifactDescriptor, AdapterError> {
    Ok(ArtifactDescriptor {
        source_unit: SourceUnit {
            id: SourceUnitId::new(value.source_unit_id),
            component: ComponentId::new(value.component_id),
            path: WorkspacePath::new(value.workspace_path),
            language: language(value.language),
            origin: source_origin(value.origin)?,
            content: Fingerprint::new(value.content_fingerprint),
            context: Fingerprint::new(value.context_fingerprint),
        },
        provenance: Provenance {
            backend: value.backend,
            backend_version: value.backend_version,
            protocol_version: value.worker_protocol_version,
            analysis_options: Fingerprint::new(value.analysis_options_fingerprint),
        },
    })
}

pub fn decode_discovery_response(
    value: worker_proto::ArtifactDiscoveryResponse,
) -> Result<ArtifactDiscoveryResponse, AdapterError> {
    Ok(ArtifactDiscoveryResponse {
        artifacts: value
            .artifacts
            .into_iter()
            .map(decode_descriptor)
            .collect::<Result<Vec<_>, _>>()?,
        next_cursor: value.next_cursor,
    })
}

fn sha256_bytes(value: &Fingerprint) -> Result<Vec<u8>, AdapterError> {
    let hex = value
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(AdapterError::Invalid("sha256 fingerprint prefix"))?;
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AdapterError::Invalid("sha256 fingerprint encoding"));
    }
    (0..32)
        .map(|index| {
            u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)
                .map_err(|_| AdapterError::Invalid("sha256 fingerprint encoding"))
        })
        .collect()
}

fn sha256_fingerprint(value: Vec<u8>) -> Result<Fingerprint, AdapterError> {
    if value.len() != 32 {
        return Err(AdapterError::Invalid("sha256 byte length"));
    }
    Ok(Fingerprint::new(format!(
        "sha256:{}",
        value
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )))
}

pub fn materialization_request(
    value: &ArtifactMaterializationRequest,
) -> worker_proto::ArtifactMaterializationRequest {
    worker_proto::ArtifactMaterializationRequest {
        workspace_root: value.workspace_root.as_str().to_owned(),
        artifact: Some(descriptor(&value.artifact)),
        staging_directory: value.staging_directory.clone(),
        blob_format_version: value.blob_format_version,
    }
}

pub fn decode_materialization_request(
    value: worker_proto::ArtifactMaterializationRequest,
) -> Result<ArtifactMaterializationRequest, AdapterError> {
    if value.staging_directory.is_empty() {
        return Err(AdapterError::Invalid("staging directory"));
    }
    if value.blob_format_version == 0 {
        return Err(AdapterError::Invalid("blob format version"));
    }
    Ok(ArtifactMaterializationRequest {
        workspace_root: WorkspacePath::new(value.workspace_root),
        artifact: decode_descriptor(
            value
                .artifact
                .ok_or(AdapterError::Missing("materialization.artifact"))?,
        )?,
        staging_directory: value.staging_directory,
        blob_format_version: value.blob_format_version,
    })
}

pub fn materialization_response(
    value: &ArtifactMaterializationResponse,
) -> Result<worker_proto::ArtifactMaterializationResponse, AdapterError> {
    if value.staged_filename.is_empty() || value.byte_length == 0 || value.blob_format_version == 0
    {
        return Err(AdapterError::Invalid("materialization completion metadata"));
    }
    Ok(worker_proto::ArtifactMaterializationResponse {
        staged_filename: value.staged_filename.clone(),
        byte_length: value.byte_length,
        sha256: sha256_bytes(&value.sha256)?,
        blob_format_version: value.blob_format_version,
    })
}

pub fn decode_materialization_response(
    value: worker_proto::ArtifactMaterializationResponse,
) -> Result<ArtifactMaterializationResponse, AdapterError> {
    if value.staged_filename.is_empty() || value.byte_length == 0 || value.blob_format_version == 0
    {
        return Err(AdapterError::Invalid("materialization completion metadata"));
    }
    Ok(ArtifactMaterializationResponse {
        staged_filename: value.staged_filename,
        byte_length: value.byte_length,
        sha256: sha256_fingerprint(value.sha256)?,
        blob_format_version: value.blob_format_version,
    })
}

fn worker_error_code(value: WorkerErrorCode) -> &'static str {
    match value {
        WorkerErrorCode::IncompatibleProtocolVersion => "incompatible_protocol_version",
        WorkerErrorCode::InvalidRequest => "invalid_request",
        WorkerErrorCode::UnsupportedCapability => "unsupported_capability",
        WorkerErrorCode::AnalysisFailed => "analysis_failed",
        WorkerErrorCode::Internal => "internal",
    }
}

fn decode_worker_error_code(value: String) -> Result<WorkerErrorCode, AdapterError> {
    match value.as_str() {
        "incompatible_protocol_version" => Ok(WorkerErrorCode::IncompatibleProtocolVersion),
        "invalid_request" => Ok(WorkerErrorCode::InvalidRequest),
        "unsupported_capability" => Ok(WorkerErrorCode::UnsupportedCapability),
        "analysis_failed" => Ok(WorkerErrorCode::AnalysisFailed),
        "internal" => Ok(WorkerErrorCode::Internal),
        other => Err(AdapterError::Unsupported(format!(
            "worker error code {other}"
        ))),
    }
}

pub fn worker_error(value: &WorkerError) -> worker_proto::Error {
    worker_proto::Error {
        code: worker_error_code(value.code).to_owned(),
        message: value.message.clone(),
        retryable: value.retryable,
        supported_protocol_version: value.supported_protocol_version,
        received_protocol_version: value.received_protocol_version,
    }
}

pub fn decode_worker_error(value: worker_proto::Error) -> Result<WorkerError, AdapterError> {
    Ok(WorkerError {
        code: decode_worker_error_code(value.code)?,
        message: value.message,
        retryable: value.retryable,
        supported_protocol_version: value.supported_protocol_version,
        received_protocol_version: value.received_protocol_version,
    })
}

pub fn manifest(value: &ProjectManifest) -> worker_proto::ProjectManifest {
    worker_proto::ProjectManifest {
        workspace: value.workspace.as_str().to_owned(),
        root: value.root.as_str().to_owned(),
        components: value
            .components
            .iter()
            .map(|component| worker_proto::Component {
                id: component.id.as_str().to_owned(),
                name: component.name.clone(),
                build_system: format!("{:?}", component.build_system).to_lowercase(),
                root: component.root.as_str().to_owned(),
                languages: component
                    .languages
                    .iter()
                    .map(|language| format!("{:?}", language).to_lowercase())
                    .collect(),
                configuration_fingerprint: component.configuration.as_str().to_owned(),
                source_sets: component
                    .source_sets
                    .iter()
                    .map(|set| worker_proto::SourceSet {
                        name: set.name.clone(),
                        source_roots: set
                            .source_roots
                            .iter()
                            .map(|path| path.as_str().to_owned())
                            .collect(),
                        generated_roots: set
                            .generated_roots
                            .iter()
                            .map(|path| path.as_str().to_owned())
                            .collect(),
                        test: set.test,
                    })
                    .collect(),
                classpath_fingerprints: component
                    .classpath
                    .iter()
                    .map(|fingerprint| fingerprint.as_str().to_owned())
                    .collect(),
                toolchain: component
                    .toolchain
                    .as_ref()
                    .map(|toolchain| worker_proto::Toolchain {
                        jvm_version: toolchain.jvm_version.clone(),
                        gradle_version: toolchain.gradle_version.clone(),
                        kotlin_version: toolchain.kotlin_version.clone(),
                    }),
                compiler_configuration_fingerprint: component
                    .compiler_configuration
                    .as_ref()
                    .map(|fingerprint| fingerprint.as_str().to_owned()),
            })
            .collect(),
        dependencies: value
            .dependencies
            .iter()
            .map(|edge| worker_proto::DependencyEdge {
                from_component_id: edge.from.as_str().to_owned(),
                scope: edge.scope.clone(),
                target: Some(match &edge.target {
                    DependencyTarget::Component { component } => {
                        worker_proto::dependency_edge::Target::ComponentId(
                            component.as_str().to_owned(),
                        )
                    }
                    DependencyTarget::Artifact { content } => {
                        worker_proto::dependency_edge::Target::ArtifactFingerprint(
                            content.as_str().to_owned(),
                        )
                    }
                }),
            })
            .collect(),
        fingerprint: value.fingerprint.as_str().to_owned(),
        provenance: Some(proto_provenance(&value.provenance)),
    }
}
