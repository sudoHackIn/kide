use std::path::{Path, PathBuf};

use anyhow::Result;
use kide_core::{
    document_from_bytes, ArtifactBlobCache, Completeness, Freshness, IndexStore, Precision,
    QueryPayload, QueryProblem, QueryResponse, QueryStatus, ResultMetadata, SymbolRecord,
    CANONICAL_SCHEMA_VERSION,
};

use super::{
    navigation::{print_query_response, print_short_symbols},
    print_response, WorkspaceContext,
};

pub(super) fn status(context: &WorkspaceContext, human_output: bool) -> Result<QueryStatus> {
    let workspace = context.path();
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
    let discovery = kide_core::discover_workspace(workspace)?;
    let configuration_status =
        store.configuration_input_status(&discovery.configuration_input_records)?;
    let mut configuration_counts = kide_core::ConfigurationInputCounts::default();
    for input in &configuration_status.inputs {
        match input.state {
            kide_core::ConfigurationInputState::Current => configuration_counts.current += 1,
            kide_core::ConfigurationInputState::Added => configuration_counts.added += 1,
            kide_core::ConfigurationInputState::Changed => configuration_counts.changed += 1,
            kide_core::ConfigurationInputState::Missing => configuration_counts.missing += 1,
        }
    }
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
    let configuration_stale = configuration_counts.added > 0
        || configuration_counts.changed > 0
        || configuration_counts.missing > 0;
    let freshness = if counts.stale > 0 || configuration_stale {
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
        configuration_schema_version: context.configuration.schema_version,
        freshness_strategy: context.configuration.freshness_strategy,
        manifest: freshness,
        source_units: counts,
        configuration_inputs: configuration_counts,
        affected_components: configuration_status.affected_components,
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
    context: &WorkspaceContext,
    query: String,
    human_output: bool,
) -> Result<QueryStatus> {
    let workspace = context.path();
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

pub(super) fn symbols(context: &WorkspaceContext, query: String, short: bool) -> Result<QueryStatus> {
    let workspace = context.path();
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
        symbols = cached_dependency_symbols(&store, workspace, &query)?;
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

fn cached_dependency_symbols(
    store: &IndexStore,
    workspace: &Path,
    qualified_name: &str,
) -> Result<Vec<SymbolRecord>> {
    let cache_root = std::env::var_os("KIDE_ARTIFACT_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join(".kide/artifact-cache"));
    let cache = ArtifactBlobCache::open(cache_root)?;
    let mut symbols = Vec::new();
    for descriptor in store.artifact_candidates_with_qualified_name(qualified_name)? {
        symbols.extend(kide_core::artifact_query::symbols_with_qualified_name(
            &cache,
            &descriptor,
            qualified_name,
        )?);
    }
    Ok(symbols)
}

#[cfg(test)]
mod tests {
    use kide_core::{
        artifact_blob_layout::ArtifactBlobLayout, artifact_proto, ArtifactBlobKey,
        ArtifactDescriptor, ComponentId, Fingerprint, Language, Provenance, SourceOrigin,
        SourceUnit, SourceUnitId, SymbolId, SymbolLocator, WORKER_PROTOCOL_VERSION,
    };
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn reads_dependency_symbol_from_compact_directory_without_a_snapshot() {
        let workspace = tempdir().expect("workspace");
        let source = SourceUnit {
            id: SourceUnitId::new("jvm:widget"),
            component: ComponentId::new("fixture:main"),
            path: kide_core::WorkspacePath::new(".kide/dependencies/widget.jar"),
            language: Language::Java,
            origin: SourceOrigin::Dependency,
            content: Fingerprint::new("sha256:widget"),
            context: Fingerprint::new("sha256:context"),
        };
        let provenance = Provenance {
            backend: "fixture".into(),
            backend_version: "1".into(),
            protocol_version: WORKER_PROTOCOL_VERSION,
            analysis_options: Fingerprint::new("sha256:options"),
        };
        let descriptor = ArtifactDescriptor {
            source_unit: source.clone(),
            provenance: provenance.clone(),
            symbol_locators: vec![SymbolLocator {
                qualified_name: "example.Widget".into(),
                symbol: SymbolId::new("java:example.Widget"),
            }],
        };
        let store = IndexStore::open(IndexStore::default_path(workspace.path())).expect("store");
        store.put_artifact_descriptor(&descriptor).expect("catalog");
        let cache =
            ArtifactBlobCache::open(workspace.path().join(".kide/artifact-cache")).expect("cache");
        let graph = artifact_proto::GraphArtifact {
            snapshots: vec![artifact_proto::GraphSnapshot {
                source_unit: Some(artifact_proto::ArtifactSourceUnit {
                    id: source.id.as_str().to_owned(),
                    component: source.component.as_str().to_owned(),
                    path: source.path.as_str().to_owned(),
                    language: "java".into(),
                    origin: "dependency".into(),
                    content_fingerprint: source.content.as_str().to_owned(),
                    context_fingerprint: source.context.as_str().to_owned(),
                }),
                provenances: vec![artifact_proto::ArtifactProvenance {
                    backend: provenance.backend.clone(),
                    backend_version: provenance.backend_version.clone(),
                    worker_protocol_version: provenance.protocol_version,
                    analysis_options_fingerprint: provenance.analysis_options.as_str().to_owned(),
                }],
                provenance_index: Some(0),
                symbols: vec![artifact_proto::ArtifactSymbol {
                    id: "java:example.Widget".into(),
                    backend_key: "example.Widget".into(),
                    backend_schema_version: 1,
                    language: "java".into(),
                    kind: "class".into(),
                    name: "Widget".into(),
                    qualified_name: Some("example.Widget".into()),
                    declaration: Some(artifact_proto::ArtifactRange { start: 0, end: 6 }),
                    name_range: Some(artifact_proto::ArtifactRange { start: 0, end: 6 }),
                    freshness: "fresh".into(),
                    completeness: "partial".into(),
                    component_id: "fixture:main".into(),
                    provenance_index: Some(0),
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        let key = ArtifactBlobKey::new(source.content.clone(), &provenance);
        cache
            .publish(&key, ArtifactBlobLayout::encode(&graph).bytes())
            .expect("publishes");

        let symbols = cached_dependency_symbols(&store, workspace.path(), "example.Widget")
            .expect("queries postings");
        assert_eq!(symbols[0].id.as_str(), "java:example.Widget");
        assert!(store.source_unit(&source.id).expect("store read").is_none());
    }
}
