use serde::{Deserialize, Serialize};

use crate::{
    Completeness, Fingerprint, Freshness, Location, Precision, Provenance, SourceOccurrence,
    SymbolId, SymbolRecord, TypeRecord, WorkspacePath, CANONICAL_SCHEMA_VERSION,
    INDEX_FORMAT_VERSION,
};

/// User intent accepted by position-oriented and symbol-oriented navigation
/// commands. The same query engine resolves all three forms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NavigationTarget {
    Location { location: Location },
    Symbol { symbol: SymbolId },
    Query { query: String },
}

/// A command-shaped public request. The CLI is a thin parser for this model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum QueryRequest {
    Index { path: WorkspacePath },
    Status,
    Symbols { query: String },
    Definition { target: NavigationTarget },
    Refs { target: NavigationTarget },
    Implementations { target: NavigationTarget },
    Callers { target: NavigationTarget },
    TypeAt { location: Location },
}

/// Terminal result state. `stale` is never silently equivalent to `ok`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryStatus {
    Ok,
    NoResult,
    Ambiguous,
    Stale,
    Unsupported,
    InvalidRequest,
    Failed,
}

/// A structured problem for agents and machine-readable command output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryProblem {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

/// Provenance and freshness attached to every semantic answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultMetadata {
    pub freshness: Freshness,
    pub completeness: Completeness,
    pub precision: Precision,
    pub index_format_version: u32,
    pub source_snapshot: Option<Fingerprint>,
    pub provenance: Vec<Provenance>,
}

impl ResultMetadata {
    pub fn empty() -> Self {
        Self {
            freshness: Freshness::Unknown,
            completeness: Completeness::Partial,
            precision: Precision::Exact,
            index_format_version: INDEX_FORMAT_VERSION,
            source_snapshot: None,
            provenance: Vec::new(),
        }
    }
}

/// Versioned JSON response envelope emitted by the CLI/API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryResponse {
    pub schema_version: u32,
    pub status: QueryStatus,
    pub result: Option<QueryPayload>,
    pub metadata: ResultMetadata,
    pub problems: Vec<QueryProblem>,
}

impl QueryResponse {
    pub fn ok(result: QueryPayload, metadata: ResultMetadata) -> Self {
        Self {
            schema_version: CANONICAL_SCHEMA_VERSION,
            status: QueryStatus::Ok,
            result: Some(result),
            metadata,
            problems: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueryPayload {
    Index(IndexResult),
    Status(StatusResult),
    Symbols {
        symbols: Vec<SymbolRecord>,
    },
    Selector {
        records: Vec<SelectorRecord>,
    },
    Definition {
        symbol: SymbolRecord,
    },
    Refs {
        references: Vec<SourceOccurrence>,
    },
    Implementations {
        symbols: Vec<SymbolRecord>,
    },
    Callers {
        calls: Vec<SourceOccurrence>,
    },
    TypeAt {
        occurrence: SourceOccurrence,
        ty: TypeRecord,
    },
}

/// One stable JSONL record emitted by a semantic selector. Navigation clients
/// consume `symbol.id`, never a rendered declaration string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectorRecord {
    pub symbol: SymbolRecord,
    pub metadata: ResultMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexResult {
    pub workspace: WorkspacePath,
    pub indexed_source_units: u64,
    pub changed_source_units: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusResult {
    pub workspace: WorkspacePath,
    /// Version of the effective configuration contract used for this status.
    pub configuration_schema_version: u32,
    /// The policy that semantic query gates must enforce for this workspace.
    pub freshness_strategy: crate::FreshnessStrategy,
    pub manifest: Freshness,
    pub source_units: IndexCounts,
    pub configuration_inputs: ConfigurationInputCounts,
    pub affected_components: Vec<crate::ComponentId>,
    pub workers_running: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigurationInputCounts {
    pub current: u64,
    pub added: u64,
    pub changed: u64,
    pub missing: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexCounts {
    pub fresh: u64,
    pub stale: u64,
    pub unknown: u64,
    pub unsupported: u64,
}

#[cfg(test)]
mod tests {
    use crate::{Completeness, Freshness, Precision};

    use super::*;

    #[test]
    fn responses_keep_an_explicit_schema_and_metadata() {
        let response = QueryResponse::ok(
            QueryPayload::Status(StatusResult {
                workspace: WorkspacePath::new("."),
                configuration_schema_version: crate::CONFIGURATION_SCHEMA_VERSION,
                freshness_strategy: crate::FreshnessStrategy::FreshOnly,
                manifest: Freshness::Fresh,
                source_units: IndexCounts {
                    fresh: 2,
                    stale: 0,
                    unknown: 0,
                    unsupported: 0,
                },
                configuration_inputs: ConfigurationInputCounts::default(),
                affected_components: Vec::new(),
                workers_running: Vec::new(),
            }),
            ResultMetadata {
                freshness: Freshness::Fresh,
                completeness: Completeness::Complete,
                precision: Precision::Exact,
                index_format_version: INDEX_FORMAT_VERSION,
                source_snapshot: None,
                provenance: Vec::new(),
            },
        );

        let encoded = serde_json::to_value(response).expect("serializes response");

        assert_eq!(encoded["schema_version"], CANONICAL_SCHEMA_VERSION);
        assert_eq!(encoded["status"], "ok");
        assert_eq!(encoded["metadata"]["freshness"], "fresh");
    }
}
