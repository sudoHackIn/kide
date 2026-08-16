use std::path::Path;

use anyhow::Result;
use kide_core::{
    CANONICAL_SCHEMA_VERSION, Completeness, Freshness, IndexStore, Precision, QueryPayload,
    QueryProblem, QueryResponse, QueryStatus, ResultMetadata, document_from_bytes,
};

use super::{
    navigation::{print_query_response, print_short_symbols},
    print_response,
};

pub(super) fn status(workspace: &Path, human_output: bool) -> Result<QueryStatus> {
    let store = IndexStore::open(IndexStore::default_path(workspace))?;
    let Some(manifest) = store.latest_manifest()? else {
        return print_query_response(QueryStatus::NoResult, None, Vec::new());
    };
    let mut counts = kide_core::IndexCounts {
        fresh: 0,
        stale: 0,
        unknown: 0,
        unsupported: 0,
    };
    for source in store.source_units()? {
        match source.origin {
            kide_core::SourceOrigin::Source | kide_core::SourceOrigin::Generated => {
                let current = std::fs::read(workspace.join(source.path.as_str()))
                    .ok()
                    .and_then(|bytes| document_from_bytes(source.path.clone(), bytes).ok());
                match current {
                    Some(current) if current.fingerprint == source.content => counts.fresh += 1,
                    Some(_) => counts.stale += 1,
                    None => counts.unknown += 1,
                }
            }
            kide_core::SourceOrigin::Dependency => counts.unknown += 1,
        }
    }
    let freshness = if counts.stale > 0 {
        Freshness::Stale
    } else if counts.unknown > 0 {
        Freshness::Unknown
    } else {
        Freshness::Fresh
    };
    let completeness = if counts.unknown == 0 {
        Completeness::Complete
    } else {
        Completeness::Partial
    };
    let mut provenance = store
        .analysis_inputs()?
        .into_iter()
        .map(|input| input.provenance)
        .collect::<Vec<_>>();
    provenance.sort_by(|left, right| {
        (
            left.backend.as_str(),
            left.backend_version.as_str(),
            left.protocol_version,
            left.analysis_options.as_str(),
        )
            .cmp(&(
                right.backend.as_str(),
                right.backend_version.as_str(),
                right.protocol_version,
                right.analysis_options.as_str(),
            ))
    });
    provenance.dedup();
    let payload = QueryPayload::Status(kide_core::StatusResult {
        workspace: manifest.root,
        manifest: freshness,
        source_units: counts,
        workers_running: Vec::new(),
    });
    if human_output {
        if let QueryPayload::Status(status) = &payload {
            println!(
                "{} fresh source units; no workers running",
                status.source_units.fresh
            );
        }
        return Ok(QueryStatus::Ok);
    }
    let response = QueryResponse {
        schema_version: CANONICAL_SCHEMA_VERSION,
        status: QueryStatus::Ok,
        result: Some(payload),
        metadata: ResultMetadata {
            freshness,
            completeness,
            precision: Precision::Exact,
            index_format_version: store.index_format_version(),
            source_snapshot: None,
            provenance,
        },
        problems: Vec::new(),
    };
    print_response(&response)?;
    Ok(QueryStatus::Ok)
}

pub(super) fn text_search(
    workspace: &Path,
    query: String,
    human_output: bool,
) -> Result<QueryStatus> {
    if query.is_empty()
        || !query
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
    {
        let response = QueryResponse {
            schema_version: CANONICAL_SCHEMA_VERSION,
            status: QueryStatus::InvalidRequest,
            result: None,
            metadata: ResultMetadata::empty(),
            problems: vec![QueryProblem {
                code: "invalid_text_query".to_owned(),
                message: "text query must be one non-empty lexical term".to_owned(),
                retryable: false,
            }],
        };
        print_response(&response)?;
        return Ok(QueryStatus::InvalidRequest);
    }
    let store = IndexStore::open(IndexStore::default_path(workspace))?;
    let matches = store.lexical_matches(workspace, &query)?;
    if human_output {
        if matches.is_empty() {
            println!("no text matches");
            return Ok(QueryStatus::NoResult);
        }
        for matched in &matches {
            println!(
                "{} {}:{}:{}",
                matched.snippet,
                matched.path.as_str(),
                matched.position.line,
                matched.position.column,
            );
        }
    } else {
        for matched in &matches {
            println!(
                "{}",
                serde_json::json!({
                    "schema_version": CANONICAL_SCHEMA_VERSION,
                    "match_kind": "lexical",
                    "path": matched.path,
                    "range": matched.range,
                    "position": matched.position,
                    "snippet": matched.snippet,
                    "freshness": "fresh",
                })
            );
        }
    }
    Ok(if matches.is_empty() {
        QueryStatus::NoResult
    } else {
        QueryStatus::Ok
    })
}

pub(super) fn symbols(workspace: &Path, query: String, short: bool) -> Result<QueryStatus> {
    let store = IndexStore::open(IndexStore::default_path(workspace))?;
    let mut symbols = store.symbols_named(&query)?;
    if symbols.is_empty() {
        symbols = store
            .symbols_with_qualified_name(&query)?
            .into_iter()
            .map(|id| store.symbol(&id))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
    }
    if symbols.is_empty() {
        symbols = store.symbols_matching_name(&query)?;
    }
    symbols.sort_by_key(|symbol| {
        (
            symbol.declaration.source_unit.as_str().to_owned(),
            symbol.name_range.bytes.start,
            symbol.id.as_str().to_owned(),
        )
    });
    let freshness = if symbols
        .iter()
        .any(|symbol| symbol.freshness == Freshness::Stale)
    {
        Freshness::Stale
    } else if symbols
        .iter()
        .any(|symbol| symbol.freshness == Freshness::Unknown)
    {
        Freshness::Unknown
    } else {
        Freshness::Fresh
    };
    let mut provenance = symbols
        .iter()
        .map(|symbol| symbol.provenance.clone())
        .collect::<Vec<_>>();
    provenance.sort_by(|left, right| {
        (
            left.backend.as_str(),
            left.backend_version.as_str(),
            left.protocol_version,
            left.analysis_options.as_str(),
        )
            .cmp(&(
                right.backend.as_str(),
                right.backend_version.as_str(),
                right.protocol_version,
                right.analysis_options.as_str(),
            ))
    });
    provenance.dedup();
    let status = match symbols.len() {
        0 => QueryStatus::NoResult,
        1 => QueryStatus::Ok,
        _ => QueryStatus::Ambiguous,
    };
    if short && !symbols.is_empty() {
        print_short_symbols(&store, workspace, &symbols)?;
        return Ok(status);
    }
    let response = QueryResponse {
        schema_version: CANONICAL_SCHEMA_VERSION,
        status,
        result: (!symbols.is_empty()).then_some(QueryPayload::Symbols { symbols }),
        metadata: ResultMetadata {
            freshness,
            completeness: Completeness::Complete,
            precision: Precision::Exact,
            index_format_version: store.index_format_version(),
            source_snapshot: None,
            provenance,
        },
        problems: if status == QueryStatus::Ambiguous {
            vec![QueryProblem {
                code: "ambiguous_symbol_query".to_owned(),
                message:
                    "symbol query matched multiple declarations; choose a returned stable symbol id"
                        .to_owned(),
                retryable: false,
            }]
        } else {
            Vec::new()
        },
    };
    print_response(&response)?;
    Ok(status)
}
