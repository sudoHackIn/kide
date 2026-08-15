//! Conversion and validation for persistent graph-artifact protobuf blobs.
//!
//! Storage types deliberately have their own schema. The explicit bridge via
//! the canonical worker snapshot representation keeps a transport-IDL change
//! from silently changing bytes already stored in the artifact cache.

use prost::Message;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{FileAnalysisSnapshot, artifact_proto, worker_proto, worker_proto_adapter};

pub const ARTIFACT_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum ArtifactProtoError {
    #[error("unsupported graph artifact format version {0}")]
    UnsupportedVersion(u32),
    #[error("graph artifact payload length does not match its envelope")]
    LengthMismatch,
    #[error("graph artifact payload checksum does not match its envelope")]
    ChecksumMismatch,
    #[error("graph artifact protobuf failed to decode: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("graph artifact cannot represent the canonical snapshot: {0}")]
    Canonical(#[from] worker_proto_adapter::AdapterError),
}

pub fn encode_snapshots(snapshots: &[FileAnalysisSnapshot]) -> Result<Vec<u8>, ArtifactProtoError> {
    let artifact = artifact_proto::GraphArtifact {
        snapshots: snapshots
            .iter()
            .map(snapshot)
            .collect::<Result<Vec<_>, _>>()?,
    };
    let payload = artifact.encode_to_vec();
    let envelope = artifact_proto::ArtifactEnvelope {
        format_version: ARTIFACT_FORMAT_VERSION,
        payload_length: payload.len() as u64,
        payload_sha256: Sha256::digest(&payload).to_vec(),
        artifact: Some(artifact),
    };
    Ok(envelope.encode_to_vec())
}

pub fn decode_snapshots(bytes: &[u8]) -> Result<Vec<FileAnalysisSnapshot>, ArtifactProtoError> {
    let envelope = artifact_proto::ArtifactEnvelope::decode(bytes)?;
    if envelope.format_version != ARTIFACT_FORMAT_VERSION {
        return Err(ArtifactProtoError::UnsupportedVersion(
            envelope.format_version,
        ));
    }
    let artifact = envelope
        .artifact
        .ok_or(ArtifactProtoError::LengthMismatch)?;
    let payload = artifact.encode_to_vec();
    if envelope.payload_length != payload.len() as u64 {
        return Err(ArtifactProtoError::LengthMismatch);
    }
    if envelope.payload_sha256 != Sha256::digest(&payload).as_slice() {
        return Err(ArtifactProtoError::ChecksumMismatch);
    }
    decode_graph_artifact(artifact)
}

pub fn decode_graph_artifact(
    artifact: artifact_proto::GraphArtifact,
) -> Result<Vec<FileAnalysisSnapshot>, ArtifactProtoError> {
    artifact
        .snapshots
        .into_iter()
        .map(decode_snapshot)
        .collect()
}

fn snapshot(
    value: &FileAnalysisSnapshot,
) -> Result<artifact_proto::GraphSnapshot, ArtifactProtoError> {
    let proto = worker_proto_adapter::file_analysis_snapshot(value)?;
    Ok(artifact_proto::GraphSnapshot {
        source_unit: proto.source_unit.map(source_unit),
        structural_fingerprint: proto.structural_fingerprint,
        public_api_fingerprint: proto.public_api_fingerprint,
        provenances: proto.provenances.into_iter().map(provenance).collect(),
        symbols: proto.symbols.into_iter().map(symbol).collect(),
        occurrences: proto.occurrences.into_iter().map(occurrence).collect(),
        references: proto.references.into_iter().map(reference).collect(),
        calls: proto.calls.into_iter().map(call).collect(),
        hierarchy: proto.hierarchy.into_iter().map(hierarchy).collect(),
        types: proto.types.into_iter().map(ty).collect(),
        diagnostics: proto.diagnostics.into_iter().map(diagnostic).collect(),
        completeness: proto.completeness,
        provenance_index: proto.provenance_index,
    })
}

fn decode_snapshot(
    value: artifact_proto::GraphSnapshot,
) -> Result<FileAnalysisSnapshot, ArtifactProtoError> {
    worker_proto_adapter::decode_file_analysis_snapshot(worker_proto::FileAnalysisSnapshot {
        source_unit: value.source_unit.map(decode_source_unit),
        structural_fingerprint: value.structural_fingerprint,
        public_api_fingerprint: value.public_api_fingerprint,
        provenances: value
            .provenances
            .into_iter()
            .map(decode_provenance)
            .collect(),
        symbols: value.symbols.into_iter().map(decode_symbol).collect(),
        occurrences: value
            .occurrences
            .into_iter()
            .map(decode_occurrence)
            .collect(),
        references: value.references.into_iter().map(decode_reference).collect(),
        calls: value.calls.into_iter().map(decode_call).collect(),
        hierarchy: value.hierarchy.into_iter().map(decode_hierarchy).collect(),
        types: value.types.into_iter().map(decode_type).collect(),
        diagnostics: value
            .diagnostics
            .into_iter()
            .map(decode_diagnostic)
            .collect(),
        completeness: value.completeness,
        provenance_index: value.provenance_index,
    })
    .map_err(Into::into)
}

fn source_unit(value: worker_proto::SourceUnit) -> artifact_proto::ArtifactSourceUnit {
    artifact_proto::ArtifactSourceUnit {
        id: value.id,
        component: value.component,
        path: value.path,
        language: value.language,
        origin: value.origin,
        content_fingerprint: value.content,
        context_fingerprint: value.context,
    }
}
fn decode_source_unit(value: artifact_proto::ArtifactSourceUnit) -> worker_proto::SourceUnit {
    worker_proto::SourceUnit {
        id: value.id,
        component: value.component,
        path: value.path,
        language: value.language,
        origin: value.origin,
        content: value.content_fingerprint,
        context: value.context_fingerprint,
    }
}
fn provenance(value: worker_proto::Provenance) -> artifact_proto::ArtifactProvenance {
    artifact_proto::ArtifactProvenance {
        backend: value.backend,
        backend_version: value.backend_version,
        worker_protocol_version: value.protocol_version,
        analysis_options_fingerprint: value.analysis_options_fingerprint,
    }
}
fn decode_provenance(value: artifact_proto::ArtifactProvenance) -> worker_proto::Provenance {
    worker_proto::Provenance {
        backend: value.backend,
        backend_version: value.backend_version,
        protocol_version: value.worker_protocol_version,
        analysis_options_fingerprint: value.analysis_options_fingerprint,
    }
}
fn range(value: worker_proto::ByteRange) -> artifact_proto::ArtifactRange {
    artifact_proto::ArtifactRange {
        start: value.start,
        end: value.end,
    }
}
fn decode_range(value: artifact_proto::ArtifactRange) -> worker_proto::ByteRange {
    worker_proto::ByteRange {
        start: value.start,
        end: value.end,
    }
}
fn location(value: worker_proto::SourceLocation) -> artifact_proto::ArtifactLocation {
    artifact_proto::ArtifactLocation {
        source_unit_index: value.source_unit_index,
        range: value.range.map(range),
    }
}
fn decode_location(value: artifact_proto::ArtifactLocation) -> worker_proto::SourceLocation {
    worker_proto::SourceLocation {
        source_unit_index: value.source_unit_index,
        range: value.range.map(decode_range),
    }
}
fn symbol(value: worker_proto::SymbolDeclaration) -> artifact_proto::ArtifactSymbol {
    artifact_proto::ArtifactSymbol {
        id: value.id,
        backend_key: value.backend_key,
        backend_schema_version: value.backend_schema_version,
        language: value.language,
        kind: value.kind,
        name: value.name,
        qualified_name: value.qualified_name,
        signature: value.signature,
        declaration: value.declaration.map(range),
        name_range: value.name_range.map(range),
        owner_id: value.owner_id,
        modifiers: value.modifiers,
        annotations: value.annotations,
        annotation_target_ids: value.annotation_target_ids,
        freshness: value.freshness,
        completeness: value.completeness,
        component_id: value.component_id,
        provenance_index: value.provenance_index,
    }
}
fn decode_symbol(value: artifact_proto::ArtifactSymbol) -> worker_proto::SymbolDeclaration {
    worker_proto::SymbolDeclaration {
        id: value.id,
        source_unit_index: 0,
        provenance_index: value.provenance_index,
        backend_key: value.backend_key,
        backend_schema_version: value.backend_schema_version,
        language: value.language,
        kind: value.kind,
        name: value.name,
        qualified_name: value.qualified_name,
        signature: value.signature,
        declaration: value.declaration.map(decode_range),
        name_range: value.name_range.map(decode_range),
        owner_id: value.owner_id,
        modifiers: value.modifiers,
        annotations: value.annotations,
        annotation_target_ids: value.annotation_target_ids,
        freshness: value.freshness,
        completeness: value.completeness,
        component_id: value.component_id,
    }
}
fn occurrence(value: worker_proto::Occurrence) -> artifact_proto::ArtifactOccurrence {
    artifact_proto::ArtifactOccurrence {
        location: value.location.map(location),
        kind: value.kind,
        enclosing_symbol_id: value.enclosing_symbol_id,
        target_symbol_id: value.target_symbol_id,
        type_id: value.type_id,
        precision: value.precision,
        freshness: value.freshness,
        completeness: value.completeness,
        provenance_index: value.provenance_index,
    }
}
fn decode_occurrence(value: artifact_proto::ArtifactOccurrence) -> worker_proto::Occurrence {
    worker_proto::Occurrence {
        location: value.location.map(decode_location),
        kind: value.kind,
        enclosing_symbol_id: value.enclosing_symbol_id,
        target_symbol_id: value.target_symbol_id,
        type_id: value.type_id,
        precision: value.precision,
        freshness: value.freshness,
        completeness: value.completeness,
        provenance_index: value.provenance_index,
    }
}
fn reference(value: worker_proto::ReferenceEdge) -> artifact_proto::ArtifactReference {
    artifact_proto::ArtifactReference {
        source_occurrence_index: value.source_occurrence_index,
        target_symbol_id: value.target_symbol_id,
        precision: value.precision,
    }
}
fn decode_reference(value: artifact_proto::ArtifactReference) -> worker_proto::ReferenceEdge {
    worker_proto::ReferenceEdge {
        source_occurrence_index: value.source_occurrence_index,
        target_symbol_id: value.target_symbol_id,
        precision: value.precision,
    }
}
fn call(value: worker_proto::CallEdge) -> artifact_proto::ArtifactCall {
    artifact_proto::ArtifactCall {
        source_occurrence_index: value.source_occurrence_index,
        target_symbol_id: value.target_symbol_id,
        caller_symbol_id: value.caller_symbol_id,
        precision: value.precision,
    }
}
fn decode_call(value: artifact_proto::ArtifactCall) -> worker_proto::CallEdge {
    worker_proto::CallEdge {
        source_occurrence_index: value.source_occurrence_index,
        target_symbol_id: value.target_symbol_id,
        caller_symbol_id: value.caller_symbol_id,
        precision: value.precision,
    }
}
fn hierarchy(value: worker_proto::HierarchyEdge) -> artifact_proto::ArtifactHierarchy {
    artifact_proto::ArtifactHierarchy {
        subtype_symbol_id: value.subtype_symbol_id,
        supertype_symbol_id: value.supertype_symbol_id,
        precision: value.precision,
        provenance_index: value.provenance_index,
    }
}
fn decode_hierarchy(value: artifact_proto::ArtifactHierarchy) -> worker_proto::HierarchyEdge {
    worker_proto::HierarchyEdge {
        subtype_symbol_id: value.subtype_symbol_id,
        supertype_symbol_id: value.supertype_symbol_id,
        precision: value.precision,
        provenance_index: value.provenance_index,
    }
}
fn ty(value: worker_proto::TypeRecord) -> artifact_proto::ArtifactType {
    artifact_proto::ArtifactType {
        id: value.id,
        language: value.language,
        display: value.display,
        backend_key: value.backend_key,
        backend_schema_version: value.backend_schema_version,
        freshness: value.freshness,
        completeness: value.completeness,
        provenance_index: value.provenance_index,
    }
}
fn decode_type(value: artifact_proto::ArtifactType) -> worker_proto::TypeRecord {
    worker_proto::TypeRecord {
        id: value.id,
        language: value.language,
        display: value.display,
        backend_key: value.backend_key,
        backend_schema_version: value.backend_schema_version,
        freshness: value.freshness,
        completeness: value.completeness,
        provenance_index: value.provenance_index,
    }
}
fn diagnostic(value: worker_proto::Diagnostic) -> artifact_proto::ArtifactDiagnostic {
    artifact_proto::ArtifactDiagnostic {
        source_unit_index: value.source_unit_index,
        range: value.range.map(range),
        severity: value.severity,
        code: value.code,
        message: value.message,
        freshness: value.freshness,
        completeness: value.completeness,
        provenance_index: value.provenance_index,
    }
}
fn decode_diagnostic(value: artifact_proto::ArtifactDiagnostic) -> worker_proto::Diagnostic {
    worker_proto::Diagnostic {
        source_unit_index: value.source_unit_index,
        range: value.range.map(decode_range),
        severity: value.severity,
        code: value.code,
        message: value.message,
        freshness: value.freshness,
        completeness: value.completeness,
        provenance_index: value.provenance_index,
    }
}
