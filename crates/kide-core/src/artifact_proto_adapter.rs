//! Conversion and validation for persistent graph-artifact protobuf blobs.
//!
//! Storage types deliberately have their own schema. The explicit bridge via
//! the canonical worker snapshot representation keeps a transport-IDL change
//! from silently changing bytes already stored in the artifact cache.

use std::collections::HashMap;

use prost::Message;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    FileAnalysisSnapshot, SymbolRecord, artifact_proto, worker_proto, worker_proto_adapter,
};

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
    let local_ordinals = snapshots
        .iter()
        .flat_map(|snapshot| snapshot.symbols.iter())
        .enumerate()
        .map(|(ordinal, symbol)| (symbol.id.as_str().to_owned(), ordinal as u32))
        .collect::<HashMap<_, _>>();
    let artifact = artifact_proto::GraphArtifact {
        snapshots: snapshots
            .iter()
            .map(|value| snapshot(value, &local_ordinals))
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
    let local_ids = artifact
        .snapshots
        .iter()
        .flat_map(|snapshot| snapshot.symbols.iter())
        .map(|symbol| symbol.id.clone())
        .collect::<Vec<_>>();
    artifact
        .snapshots
        .into_iter()
        .map(|snapshot| decode_snapshot(snapshot, &local_ids))
        .collect()
}

/// Reconstructs one canonical declaration from its independently readable
/// ordinal detail block. The block carries snapshot-level defaults, so this
/// does not require GraphFacts or a descriptor supplied by the caller.
pub fn decode_symbol_detail(
    block: artifact_proto::ArtifactSymbolDetailBlock,
    ordinal: u32,
) -> Result<SymbolRecord, ArtifactProtoError> {
    let index = ordinal
        .checked_sub(block.first_symbol_ordinal)
        .ok_or(ArtifactProtoError::LengthMismatch)? as usize;
    let detail = block
        .entries
        .get(index)
        .ok_or(ArtifactProtoError::LengthMismatch)?;
    let defaults = block.defaults.ok_or(ArtifactProtoError::LengthMismatch)?;
    let source_unit = defaults
        .source_unit
        .ok_or(ArtifactProtoError::LengthMismatch)?;
    let provenance_index = detail
        .provenance_index
        .or(defaults.provenance_index)
        .unwrap_or(0) as usize;
    let provenance = defaults
        .provenances
        .get(provenance_index)
        .ok_or(ArtifactProtoError::LengthMismatch)?;
    let string = |inline: String, index: Option<u32>| -> Result<String, ArtifactProtoError> {
        index
            .map(|index| {
                defaults
                    .string_table
                    .get(index as usize)
                    .cloned()
                    .ok_or(ArtifactProtoError::LengthMismatch)
            })
            .unwrap_or(Ok(inline))
    };
    let strings =
        |inline: Vec<String>, indexes: Vec<u32>| -> Result<Vec<String>, ArtifactProtoError> {
            if indexes.is_empty() {
                return Ok(inline);
            }
            indexes
                .into_iter()
                .map(|index| {
                    defaults
                        .string_table
                        .get(index as usize)
                        .cloned()
                        .ok_or(ArtifactProtoError::LengthMismatch)
                })
                .collect()
        };
    let symbol = artifact_proto::ArtifactSymbol {
        id: detail.id.clone(),
        backend_key: string(detail.backend_key.clone(), detail.backend_key_string_index)?,
        backend_schema_version: detail.backend_schema_version,
        language: detail.language.clone().unwrap_or(defaults.language),
        kind: string(detail.kind.clone(), detail.kind_string_index)?,
        name: string(detail.name.clone(), detail.name_string_index)?,
        qualified_name: detail
            .qualified_name_string_index
            .map(|index| string(String::new(), Some(index)))
            .transpose()?
            .or(detail.qualified_name.clone()),
        signature: detail
            .signature_string_index
            .map(|index| string(String::new(), Some(index)))
            .transpose()?
            .or(detail.signature.clone()),
        declaration: detail.declaration,
        name_range: detail.name_range,
        owner_id: detail
            .owner_id_string_index
            .map(|index| string(String::new(), Some(index)))
            .transpose()?
            .or(detail.owner_id.clone()),
        modifiers: strings(
            detail.modifiers.clone(),
            detail.modifier_string_indexes.clone(),
        )?,
        applied_symbol_ids: strings(
            detail.applied_symbol_ids.clone(),
            detail.applied_symbol_string_indexes.clone(),
        )?,
        freshness: detail.freshness.clone().unwrap_or(defaults.freshness),
        completeness: detail.completeness.clone().unwrap_or(defaults.completeness),
        component_id: detail.component_id.clone().unwrap_or(defaults.component_id),
        provenance_index: None,
        owner_symbol_ordinal: None,
    };
    let local_ids = vec![symbol.id.clone()];
    let snapshot = decode_snapshot(
        artifact_proto::GraphSnapshot {
            source_unit: Some(source_unit),
            provenances: vec![provenance.clone()],
            symbols: vec![symbol],
            completeness: "partial".to_owned(),
            provenance_index: Some(0),
            ..Default::default()
        },
        &local_ids,
    )?;
    snapshot
        .symbols
        .into_iter()
        .next()
        .ok_or(ArtifactProtoError::LengthMismatch)
}

fn snapshot(
    value: &FileAnalysisSnapshot,
    local_ordinals: &HashMap<String, u32>,
) -> Result<artifact_proto::GraphSnapshot, ArtifactProtoError> {
    let proto = worker_proto_adapter::file_analysis_snapshot(value)?;
    Ok(artifact_proto::GraphSnapshot {
        source_unit: proto.source_unit.map(source_unit),
        structural_fingerprint: proto.structural_fingerprint,
        public_api_fingerprint: proto.public_api_fingerprint,
        provenances: proto.provenances.into_iter().map(provenance).collect(),
        symbols: proto
            .symbols
            .into_iter()
            .map(|value| symbol(value, proto.provenance_index, local_ordinals))
            .collect(),
        occurrences: proto
            .occurrences
            .into_iter()
            .map(|value| occurrence(value, proto.provenance_index, local_ordinals))
            .collect(),
        references: proto
            .references
            .into_iter()
            .map(|value| reference(value, local_ordinals))
            .collect(),
        calls: proto
            .calls
            .into_iter()
            .map(|value| call(value, local_ordinals))
            .collect(),
        hierarchy: proto
            .hierarchy
            .into_iter()
            .map(|value| hierarchy(value, proto.provenance_index, local_ordinals))
            .collect(),
        types: proto
            .types
            .into_iter()
            .map(|value| ty(value, proto.provenance_index))
            .collect(),
        diagnostics: proto
            .diagnostics
            .into_iter()
            .map(|value| diagnostic(value, proto.provenance_index))
            .collect(),
        completeness: proto.completeness,
        provenance_index: Some(proto.provenance_index),
    })
}

fn decode_snapshot(
    value: artifact_proto::GraphSnapshot,
    local_ids: &[String],
) -> Result<FileAnalysisSnapshot, ArtifactProtoError> {
    let snapshot_provenance_index = value
        .provenance_index
        .ok_or(ArtifactProtoError::LengthMismatch)?;
    worker_proto_adapter::decode_file_analysis_snapshot(worker_proto::FileAnalysisSnapshot {
        source_unit: value.source_unit.map(decode_source_unit),
        structural_fingerprint: value.structural_fingerprint,
        public_api_fingerprint: value.public_api_fingerprint,
        provenances: value
            .provenances
            .into_iter()
            .map(decode_provenance)
            .collect(),
        symbols: value
            .symbols
            .into_iter()
            .map(|symbol| decode_symbol(symbol, snapshot_provenance_index, local_ids))
            .collect(),
        applications: vec![],
        occurrences: value
            .occurrences
            .into_iter()
            .map(|occurrence| decode_occurrence(occurrence, snapshot_provenance_index, local_ids))
            .collect(),
        references: value
            .references
            .into_iter()
            .map(|edge| decode_reference(edge, local_ids))
            .collect(),
        calls: value
            .calls
            .into_iter()
            .map(|edge| decode_call(edge, local_ids))
            .collect(),
        hierarchy: value
            .hierarchy
            .into_iter()
            .map(|edge| decode_hierarchy(edge, snapshot_provenance_index, local_ids))
            .collect(),
        types: value
            .types
            .into_iter()
            .map(|record| decode_type(record, snapshot_provenance_index))
            .collect(),
        diagnostics: value
            .diagnostics
            .into_iter()
            .map(|diagnostic| decode_diagnostic(diagnostic, snapshot_provenance_index))
            .collect(),
        completeness: value.completeness,
        provenance_index: snapshot_provenance_index,
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
fn inherited_provenance_index(value: u32, snapshot: u32) -> Option<u32> {
    (value != snapshot).then_some(value)
}

fn local_ordinal(value: &str, ordinals: &HashMap<String, u32>) -> Option<u32> {
    ordinals.get(value).copied()
}
fn local_id(
    inline: String,
    ordinal: Option<u32>,
    ids: &[String],
) -> Result<String, ArtifactProtoError> {
    ordinal
        .map(|ordinal| {
            ids.get(ordinal as usize)
                .cloned()
                .ok_or(ArtifactProtoError::LengthMismatch)
        })
        .unwrap_or(Ok(inline))
}
fn local_optional(inline: Option<String>, ordinal: Option<u32>, ids: &[String]) -> Option<String> {
    ordinal
        .and_then(|ordinal| ids.get(ordinal as usize).cloned())
        .or(inline)
}

fn symbol(
    value: worker_proto::SymbolDeclaration,
    snapshot_provenance_index: u32,
    ordinals: &HashMap<String, u32>,
) -> artifact_proto::ArtifactSymbol {
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
        owner_id: value
            .owner_id
            .as_ref()
            .filter(|id| local_ordinal(id, ordinals).is_none())
            .cloned(),
        modifiers: value.modifiers,
        applied_symbol_ids: value.applied_symbol_ids,
        freshness: value.freshness,
        completeness: value.completeness,
        component_id: value.component_id,
        provenance_index: inherited_provenance_index(
            value.provenance_index,
            snapshot_provenance_index,
        ),
        owner_symbol_ordinal: value
            .owner_id
            .as_deref()
            .and_then(|id| local_ordinal(id, ordinals)),
    }
}
fn decode_symbol(
    value: artifact_proto::ArtifactSymbol,
    snapshot_provenance_index: u32,
    ids: &[String],
) -> worker_proto::SymbolDeclaration {
    worker_proto::SymbolDeclaration {
        id: value.id,
        source_unit_index: 0,
        provenance_index: value.provenance_index.unwrap_or(snapshot_provenance_index),
        backend_key: value.backend_key,
        backend_schema_version: value.backend_schema_version,
        language: value.language,
        kind: value.kind,
        name: value.name,
        qualified_name: value.qualified_name,
        signature: value.signature,
        declaration: value.declaration.map(decode_range),
        name_range: value.name_range.map(decode_range),
        owner_id: local_optional(value.owner_id, value.owner_symbol_ordinal, ids),
        modifiers: value.modifiers,
        applied_symbol_ids: value.applied_symbol_ids,
        freshness: value.freshness,
        completeness: value.completeness,
        component_id: value.component_id,
    }
}
fn occurrence(
    value: worker_proto::Occurrence,
    snapshot_provenance_index: u32,
    ordinals: &HashMap<String, u32>,
) -> artifact_proto::ArtifactOccurrence {
    artifact_proto::ArtifactOccurrence {
        location: value.location.map(location),
        kind: value.kind,
        enclosing_symbol_id: value
            .enclosing_symbol_id
            .as_ref()
            .filter(|id| local_ordinal(id, ordinals).is_none())
            .cloned(),
        target_symbol_id: value
            .target_symbol_id
            .as_ref()
            .filter(|id| local_ordinal(id, ordinals).is_none())
            .cloned(),
        type_id: value.type_id,
        precision: value.precision,
        freshness: value.freshness,
        completeness: value.completeness,
        provenance_index: inherited_provenance_index(
            value.provenance_index,
            snapshot_provenance_index,
        ),
        enclosing_symbol_ordinal: value
            .enclosing_symbol_id
            .as_deref()
            .and_then(|id| local_ordinal(id, ordinals)),
        target_symbol_ordinal: value
            .target_symbol_id
            .as_deref()
            .and_then(|id| local_ordinal(id, ordinals)),
    }
}
fn decode_occurrence(
    value: artifact_proto::ArtifactOccurrence,
    snapshot_provenance_index: u32,
    ids: &[String],
) -> worker_proto::Occurrence {
    worker_proto::Occurrence {
        location: value.location.map(decode_location),
        kind: value.kind,
        enclosing_symbol_id: local_optional(
            value.enclosing_symbol_id,
            value.enclosing_symbol_ordinal,
            ids,
        ),
        target_symbol_id: local_optional(value.target_symbol_id, value.target_symbol_ordinal, ids),
        type_id: value.type_id,
        precision: value.precision,
        freshness: value.freshness,
        completeness: value.completeness,
        provenance_index: value.provenance_index.unwrap_or(snapshot_provenance_index),
    }
}
fn reference(
    value: worker_proto::ReferenceEdge,
    ordinals: &HashMap<String, u32>,
) -> artifact_proto::ArtifactReference {
    let target_symbol_ordinal = local_ordinal(&value.target_symbol_id, ordinals);
    artifact_proto::ArtifactReference {
        source_occurrence_index: value.source_occurrence_index,
        target_symbol_id: if target_symbol_ordinal.is_none() {
            value.target_symbol_id.clone()
        } else {
            String::new()
        },
        precision: value.precision,
        target_symbol_ordinal,
    }
}
fn decode_reference(
    value: artifact_proto::ArtifactReference,
    ids: &[String],
) -> worker_proto::ReferenceEdge {
    worker_proto::ReferenceEdge {
        source_occurrence_index: value.source_occurrence_index,
        target_symbol_id: local_id(value.target_symbol_id, value.target_symbol_ordinal, ids)
            .unwrap_or_default(),
        precision: value.precision,
    }
}
fn call(
    value: worker_proto::CallEdge,
    ordinals: &HashMap<String, u32>,
) -> artifact_proto::ArtifactCall {
    let target_symbol_ordinal = local_ordinal(&value.target_symbol_id, ordinals);
    artifact_proto::ArtifactCall {
        source_occurrence_index: value.source_occurrence_index,
        target_symbol_id: if target_symbol_ordinal.is_none() {
            value.target_symbol_id.clone()
        } else {
            String::new()
        },
        caller_symbol_id: value
            .caller_symbol_id
            .as_ref()
            .filter(|id| local_ordinal(id, ordinals).is_none())
            .cloned(),
        precision: value.precision,
        target_symbol_ordinal,
        caller_symbol_ordinal: value
            .caller_symbol_id
            .as_deref()
            .and_then(|id| local_ordinal(id, ordinals)),
    }
}
fn decode_call(value: artifact_proto::ArtifactCall, ids: &[String]) -> worker_proto::CallEdge {
    worker_proto::CallEdge {
        source_occurrence_index: value.source_occurrence_index,
        target_symbol_id: local_id(value.target_symbol_id, value.target_symbol_ordinal, ids)
            .unwrap_or_default(),
        caller_symbol_id: local_optional(value.caller_symbol_id, value.caller_symbol_ordinal, ids),
        precision: value.precision,
    }
}
fn hierarchy(
    value: worker_proto::HierarchyEdge,
    snapshot_provenance_index: u32,
    ordinals: &HashMap<String, u32>,
) -> artifact_proto::ArtifactHierarchy {
    let subtype_symbol_ordinal = local_ordinal(&value.subtype_symbol_id, ordinals);
    let supertype_symbol_ordinal = local_ordinal(&value.supertype_symbol_id, ordinals);
    artifact_proto::ArtifactHierarchy {
        subtype_symbol_id: if subtype_symbol_ordinal.is_none() {
            value.subtype_symbol_id.clone()
        } else {
            String::new()
        },
        supertype_symbol_id: if supertype_symbol_ordinal.is_none() {
            value.supertype_symbol_id.clone()
        } else {
            String::new()
        },
        precision: value.precision,
        provenance_index: inherited_provenance_index(
            value.provenance_index,
            snapshot_provenance_index,
        ),
        subtype_symbol_ordinal,
        supertype_symbol_ordinal,
    }
}
fn decode_hierarchy(
    value: artifact_proto::ArtifactHierarchy,
    snapshot_provenance_index: u32,
    ids: &[String],
) -> worker_proto::HierarchyEdge {
    worker_proto::HierarchyEdge {
        subtype_symbol_id: local_id(value.subtype_symbol_id, value.subtype_symbol_ordinal, ids)
            .unwrap_or_default(),
        supertype_symbol_id: local_id(
            value.supertype_symbol_id,
            value.supertype_symbol_ordinal,
            ids,
        )
        .unwrap_or_default(),
        precision: value.precision,
        provenance_index: value.provenance_index.unwrap_or(snapshot_provenance_index),
    }
}
fn ty(
    value: worker_proto::TypeRecord,
    snapshot_provenance_index: u32,
) -> artifact_proto::ArtifactType {
    artifact_proto::ArtifactType {
        id: value.id,
        language: value.language,
        display: value.display,
        backend_key: value.backend_key,
        backend_schema_version: value.backend_schema_version,
        freshness: value.freshness,
        completeness: value.completeness,
        provenance_index: inherited_provenance_index(
            value.provenance_index,
            snapshot_provenance_index,
        ),
    }
}
fn decode_type(
    value: artifact_proto::ArtifactType,
    snapshot_provenance_index: u32,
) -> worker_proto::TypeRecord {
    worker_proto::TypeRecord {
        id: value.id,
        language: value.language,
        display: value.display,
        backend_key: value.backend_key,
        backend_schema_version: value.backend_schema_version,
        freshness: value.freshness,
        completeness: value.completeness,
        provenance_index: value.provenance_index.unwrap_or(snapshot_provenance_index),
    }
}
fn diagnostic(
    value: worker_proto::Diagnostic,
    snapshot_provenance_index: u32,
) -> artifact_proto::ArtifactDiagnostic {
    artifact_proto::ArtifactDiagnostic {
        source_unit_index: value.source_unit_index,
        range: value.range.map(range),
        severity: value.severity,
        code: value.code,
        message: value.message,
        freshness: value.freshness,
        completeness: value.completeness,
        provenance_index: inherited_provenance_index(
            value.provenance_index,
            snapshot_provenance_index,
        ),
    }
}
fn decode_diagnostic(
    value: artifact_proto::ArtifactDiagnostic,
    snapshot_provenance_index: u32,
) -> worker_proto::Diagnostic {
    worker_proto::Diagnostic {
        source_unit_index: value.source_unit_index,
        range: value.range.map(decode_range),
        severity: value.severity,
        code: value.code,
        message: value.message,
        freshness: value.freshness,
        completeness: value.completeness,
        provenance_index: value.provenance_index.unwrap_or(snapshot_provenance_index),
    }
}

#[cfg(test)]
mod tests {
    use prost::Message;

    use super::*;

    #[derive(Clone, PartialEq, Message)]
    struct LegacySymbolPostings {
        #[prost(message, repeated, tag = "1")]
        entries: Vec<LegacySymbolPosting>,
    }

    #[derive(Clone, PartialEq, Message)]
    struct LegacySymbolPosting {
        #[prost(uint32, tag = "1")]
        source_unit_index: u32,
        #[prost(message, optional, tag = "2")]
        symbol: Option<artifact_proto::ArtifactSymbol>,
    }

    fn graph(provenance_index: Option<u32>) -> artifact_proto::GraphArtifact {
        artifact_proto::GraphArtifact {
            snapshots: vec![artifact_proto::GraphSnapshot {
                source_unit: Some(artifact_proto::ArtifactSourceUnit {
                    id: "jvm:sha256:fixture".into(),
                    component: "fixture:main".into(),
                    path: ".kide/dependencies/fixture.jar".into(),
                    language: "java".into(),
                    origin: "dependency".into(),
                    content_fingerprint: "sha256:fixture".into(),
                    context_fingerprint: "sha256:context".into(),
                }),
                provenances: vec![artifact_proto::ArtifactProvenance {
                    backend: "fixture".into(),
                    backend_version: "1".into(),
                    worker_protocol_version: 1,
                    analysis_options_fingerprint: "sha256:options".into(),
                }],
                symbols: (0..512)
                    .map(|ordinal| artifact_proto::ArtifactSymbol {
                        id: format!("java:fixture.Symbol{ordinal}"),
                        backend_key: format!("fixture.Symbol{ordinal}"),
                        backend_schema_version: 1,
                        language: "java".into(),
                        kind: "class".into(),
                        name: format!("Symbol{ordinal}"),
                        qualified_name: Some(format!("fixture.Symbol{ordinal}")),
                        declaration: Some(artifact_proto::ArtifactRange {
                            start: ordinal,
                            end: ordinal + 1,
                        }),
                        name_range: Some(artifact_proto::ArtifactRange {
                            start: ordinal,
                            end: ordinal + 1,
                        }),
                        applied_symbol_ids: vec!["jvm:annotation:fixture.Repeated".into()],
                        freshness: "fresh".into(),
                        completeness: "complete".into(),
                        component_id: "fixture:main".into(),
                        provenance_index,
                        ..Default::default()
                    })
                    .collect(),
                completeness: "complete".into(),
                provenance_index: Some(0),
                ..Default::default()
            }],
        }
    }

    #[test]
    fn dependency_facts_inherit_snapshot_provenance_and_shrink_graph_bytes() {
        let compact = graph(None);
        let redundant = graph(Some(0));

        let compact_bytes = compact.encode_to_vec();
        let redundant_bytes = redundant.encode_to_vec();
        assert!(compact_bytes.len() < redundant_bytes.len());
        eprintln!(
            "artifact provenance compaction: {} -> {} bytes ({:.1}% smaller)",
            redundant_bytes.len(),
            compact_bytes.len(),
            (1.0 - compact_bytes.len() as f64 / redundant_bytes.len() as f64) * 100.0,
        );
        assert!(
            redundant_bytes.len() - compact_bytes.len() >= 1_000,
            "compact={} redundant={}",
            compact_bytes.len(),
            redundant_bytes.len()
        );

        let decoded = decode_graph_artifact(compact).expect("compact graph decodes");
        assert!(
            decoded[0]
                .symbols
                .iter()
                .all(|symbol| symbol.provenance == decoded[0].provenance)
        );
        assert_eq!(
            decode_graph_artifact(redundant).expect("explicit overrides decode"),
            decoded
        );
    }

    #[test]
    fn ordinal_symbol_postings_shrink_the_dependency_blob() {
        let graph = graph(None);
        let compact = crate::artifact_blob_layout::ArtifactBlobLayout::encode(&graph);
        let new_postings = artifact_proto::ArtifactSymbolPostings {
            entries: graph.snapshots[0]
                .symbols
                .iter()
                .enumerate()
                .map(|(ordinal, _)| artifact_proto::ArtifactSymbolPosting {
                    source_unit_index: 0,
                    symbol_ordinal: ordinal as u32,
                })
                .collect(),
        }
        .encode_to_vec();
        let legacy_postings = LegacySymbolPostings {
            entries: graph.snapshots[0]
                .symbols
                .iter()
                .map(|symbol| LegacySymbolPosting {
                    source_unit_index: 0,
                    symbol: Some(symbol.clone()),
                })
                .collect(),
        }
        .encode_to_vec();
        let legacy_size = compact.bytes().len() - new_postings.len() + legacy_postings.len();
        eprintln!(
            "artifact ordinal postings: {} -> {} bytes ({:.1}% smaller)",
            legacy_size,
            compact.bytes().len(),
            (1.0 - compact.bytes().len() as f64 / legacy_size as f64) * 100.0,
        );
        assert!(compact.bytes().len() < legacy_size);
        assert!(legacy_size - compact.bytes().len() >= 50_000);
    }

    #[test]
    fn detail_block_interns_repeated_long_strings_and_decodes_without_extra_io() {
        let graph = graph(None);
        let layout = crate::artifact_blob_layout::ArtifactBlobLayout::encode(&graph);
        let block = layout
            .symbol_detail_block(0)
            .expect("reads one bounded block");
        assert!(
            block
                .defaults
                .as_ref()
                .expect("defaults")
                .string_table
                .iter()
                .any(|value| value == "jvm:annotation:fixture.Repeated")
        );
        assert!(block.entries[0].applied_symbol_ids.is_empty());
        assert_eq!(block.entries[0].applied_symbol_string_indexes.len(), 1);
        let decoded = decode_symbol_detail(block, 0).expect("decodes interned symbol");
        assert_eq!(
            decoded
                .applied_symbols
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>(),
            graph.snapshots[0].symbols[0]
                .applied_symbol_ids
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn interned_detail_strings_shrink_a_bounded_block() {
        let graph = graph(None);
        let layout = crate::artifact_blob_layout::ArtifactBlobLayout::encode(&graph);
        let interned = layout.symbol_detail_block(0).expect("reads interned block");
        let mut inline = interned.clone();
        let repeated = inline.defaults.as_ref().expect("defaults").string_table[0].clone();
        inline
            .defaults
            .as_mut()
            .expect("defaults")
            .string_table
            .clear();
        for entry in &mut inline.entries {
            entry.applied_symbol_ids = vec![repeated.clone()];
            entry.applied_symbol_string_indexes.clear();
        }
        let interned_bytes = interned.encode_to_vec();
        let inline_bytes = inline.encode_to_vec();
        eprintln!(
            "artifact detail string interning: {} -> {} bytes ({:.1}% smaller)",
            inline_bytes.len(),
            interned_bytes.len(),
            (1.0 - interned_bytes.len() as f64 / inline_bytes.len() as f64) * 100.0,
        );
        assert!(interned_bytes.len() < inline_bytes.len());
    }

    #[test]
    fn local_graph_edge_ordinals_shrink_and_restore_symbol_ids() {
        let mut inline = graph(None);
        inline.snapshots[0].hierarchy = (1..512)
            .map(|ordinal| artifact_proto::ArtifactHierarchy {
                subtype_symbol_id: format!("java:fixture.Symbol{ordinal}"),
                supertype_symbol_id: format!("java:fixture.Symbol{}", ordinal - 1),
                precision: "exact".into(),
                provenance_index: None,
                ..Default::default()
            })
            .collect();
        let mut ordinal = inline.clone();
        for (index, edge) in ordinal.snapshots[0].hierarchy.iter_mut().enumerate() {
            edge.subtype_symbol_id.clear();
            edge.supertype_symbol_id.clear();
            edge.subtype_symbol_ordinal = Some(index as u32 + 1);
            edge.supertype_symbol_ordinal = Some(index as u32);
        }
        let inline_bytes = inline.encode_to_vec();
        let ordinal_bytes = ordinal.encode_to_vec();
        eprintln!(
            "artifact local graph ordinals: {} -> {} bytes ({:.1}% smaller)",
            inline_bytes.len(),
            ordinal_bytes.len(),
            (1.0 - ordinal_bytes.len() as f64 / inline_bytes.len() as f64) * 100.0,
        );
        assert!(ordinal_bytes.len() < inline_bytes.len());
        assert_eq!(
            decode_graph_artifact(ordinal).expect("ordinal graph decodes"),
            decode_graph_artifact(inline).expect("inline graph decodes")
        );
    }
}
