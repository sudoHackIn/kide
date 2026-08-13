//! Versioned messages exchanged between KIDE Core and disposable workers.
//!
//! The protocol carries normalized data only. Compiler sessions, PSI nodes,
//! and build-tool objects stay inside a worker and die with that process.

use serde::{Deserialize, Serialize};

use crate::{
    CallEdge, Completeness, DiagnosticRecord, Fingerprint, HierarchyEdge, Language,
    ProjectManifest, ReferenceEdge, SourceOccurrence, SourceUnit, SourceUnitId, SymbolRecord,
    TypeRecord, WorkspaceId, WorkspacePath,
};

/// First version of the Core-to-worker wire protocol.
pub const WORKER_PROTOCOL_VERSION: u32 = 1;

/// A named feature a worker can advertise before Core opens an analysis session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerCapability {
    Handshake,
    ProjectManifest,
    FileAnalysisSnapshot,
    AnalysisDelta,
}

/// Static identity of a worker implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerIdentity {
    pub backend: String,
    pub backend_version: String,
}

/// Static worker metadata returned by the handshake. It must be available
/// without importing a build or creating a compiler analysis session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerCapabilities {
    pub identity: WorkerIdentity,
    pub protocol_version: u32,
    pub languages: Vec<Language>,
    pub capabilities: Vec<WorkerCapability>,
}

/// A request whose only purpose is compatibility and static discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandshakeRequest {
    pub core_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandshakeResponse {
    pub capabilities: WorkerCapabilities,
}

/// Asks a build-system worker to describe a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectManifestRequest {
    pub workspace_root: WorkspacePath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectManifestResponse {
    pub manifest: ProjectManifest,
}

/// Facts requested from a language worker for every source unit in a batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisFact {
    Symbols,
    Occurrences,
    References,
    Calls,
    Hierarchy,
    Types,
}

/// Core supplies every input needed for a cold worker to analyze source units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalyzeBatchRequest {
    pub workspace: WorkspaceId,
    pub project_fingerprint: Fingerprint,
    pub requested_facts: Vec<AnalysisFact>,
    pub source_units: Vec<SourceUnit>,
}

/// A complete replacement fact set for exactly one source-unit content snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileAnalysisSnapshot {
    pub source_unit: SourceUnit,
    #[serde(default)]
    pub structural_fingerprint: Option<Fingerprint>,
    #[serde(default)]
    pub public_api_fingerprint: Option<Fingerprint>,
    pub symbols: Vec<SymbolRecord>,
    pub occurrences: Vec<SourceOccurrence>,
    pub references: Vec<ReferenceEdge>,
    pub calls: Vec<CallEdge>,
    pub hierarchy: Vec<HierarchyEdge>,
    pub types: Vec<TypeRecord>,
    pub diagnostics: Vec<DiagnosticRecord>,
    pub completeness: Completeness,
    pub provenance: crate::Provenance,
}

/// A batch response permits a cold worker to amortize startup across files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisBatchResponse {
    pub snapshots: Vec<FileAnalysisSnapshot>,
}

/// A conservative incremental update. `snapshot: None` removes a source unit;
/// otherwise Core replaces all persisted facts for that source-unit snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisDelta {
    pub source_unit: SourceUnitId,
    pub previous_content: Option<Fingerprint>,
    pub snapshot: Option<FileAnalysisSnapshot>,
}

/// Stable machine-readable failure codes returned through the protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerErrorCode {
    IncompatibleProtocolVersion,
    InvalidRequest,
    UnsupportedCapability,
    AnalysisFailed,
    Internal,
}

/// A structured failure that lets Core decide whether retrying is useful.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerError {
    pub code: WorkerErrorCode,
    pub message: String,
    pub retryable: bool,
    pub supported_protocol_version: u32,
    pub received_protocol_version: Option<u32>,
}

impl WorkerError {
    pub fn incompatible_protocol_version(received_protocol_version: u32) -> Self {
        Self {
            code: WorkerErrorCode::IncompatibleProtocolVersion,
            message: format!(
                "worker protocol version {received_protocol_version} is incompatible; supported version is {WORKER_PROTOCOL_VERSION}"
            ),
            retryable: false,
            supported_protocol_version: WORKER_PROTOCOL_VERSION,
            received_protocol_version: Some(received_protocol_version),
        }
    }
}

/// One NDJSON object. `request_id` correlates an error or response with the
/// Core request that caused it; it is unique within one worker process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerEnvelope {
    pub protocol_version: u32,
    pub request_id: String,
    #[serde(flatten)]
    pub message: WorkerMessage,
}

impl WorkerEnvelope {
    pub fn new(request_id: impl Into<String>, message: WorkerMessage) -> Self {
        Self {
            protocol_version: WORKER_PROTOCOL_VERSION,
            request_id: request_id.into(),
            message,
        }
    }

    /// Reject a version before interpreting the payload.
    pub fn validate_protocol_version(&self) -> Result<(), WorkerError> {
        ensure_compatible_protocol_version(self.protocol_version)
    }
}

/// The complete MVP message vocabulary. New variants require a protocol
/// version bump, so v1 workers fail safely rather than guessing semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum WorkerMessage {
    HandshakeRequest(HandshakeRequest),
    HandshakeResponse(HandshakeResponse),
    ProjectManifestRequest(ProjectManifestRequest),
    ProjectManifestResponse(ProjectManifestResponse),
    AnalyzeBatchRequest(AnalyzeBatchRequest),
    AnalysisBatchResponse(AnalysisBatchResponse),
    /// Kept behind an indirection so one rare, full-file delta does not make
    /// every handshake and batch message as large as the delta payload.
    /// `Box` is transparent to serde, therefore the NDJSON protocol is
    /// unchanged.
    AnalysisDelta(Box<AnalysisDelta>),
    Error(WorkerError),
}

/// Performs the compatibility rule shared by Core and every worker.
pub fn ensure_compatible_protocol_version(version: u32) -> Result<(), WorkerError> {
    if version == WORKER_PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(WorkerError::incompatible_protocol_version(version))
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, mem::size_of, path::PathBuf};

    use serde_json::Value;

    use super::*;

    const FIXTURES: &[&str] = &[
        "handshake-request.json",
        "handshake-response.json",
        "project-manifest-request.json",
        "project-manifest-response.json",
        "analyze-batch-request.json",
        "analysis-batch-response.json",
        "analysis-delta.json",
        "error.json",
    ];

    fn fixture(name: &str) -> String {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../protocol/fixtures")
            .join(name);
        fs::read_to_string(path).expect("reads protocol fixture")
    }

    #[test]
    fn every_mvp_message_round_trips_its_golden_fixture() {
        for name in FIXTURES {
            let expected: Value = serde_json::from_str(&fixture(name)).expect("valid fixture JSON");
            let envelope: WorkerEnvelope = serde_json::from_value(expected.clone())
                .unwrap_or_else(|error| panic!("{name} decodes: {error}"));

            assert_eq!(
                serde_json::to_value(envelope).expect("serializes message"),
                expected,
                "{name} preserves the wire contract"
            );
        }
    }

    #[test]
    fn message_size_is_bounded_by_the_largest_non_delta_batch_payload() {
        let largest_non_delta_payload = [
            size_of::<HandshakeRequest>(),
            size_of::<HandshakeResponse>(),
            size_of::<ProjectManifestRequest>(),
            size_of::<ProjectManifestResponse>(),
            size_of::<AnalyzeBatchRequest>(),
            size_of::<AnalysisBatchResponse>(),
            size_of::<WorkerError>(),
        ]
        .into_iter()
        .max()
        .expect("payload sizes are present");

        assert!(size_of::<WorkerMessage>() <= largest_non_delta_payload);
    }

    #[test]
    fn incompatible_version_is_a_structured_non_retryable_error() {
        let error = ensure_compatible_protocol_version(WORKER_PROTOCOL_VERSION + 1)
            .expect_err("future protocol must be rejected");

        assert_eq!(error.code, WorkerErrorCode::IncompatibleProtocolVersion);
        assert!(!error.retryable);
        assert_eq!(error.received_protocol_version, Some(2));
    }

    #[test]
    fn batch_response_carries_multiple_source_snapshots() {
        let envelope: WorkerEnvelope =
            serde_json::from_str(&fixture("analysis-batch-response.json"))
                .expect("batch fixture decodes");

        let WorkerMessage::AnalysisBatchResponse(response) = envelope.message else {
            panic!("expected analysis batch response");
        };
        assert_eq!(response.snapshots.len(), 2);
    }
}
