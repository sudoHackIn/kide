use std::path::Path;

use anyhow::{Result, bail};
use kide_core::{
    ArtifactBlobCache, CANONICAL_SCHEMA_VERSION, IndexStore, QueryPayload, QueryProblem,
    QueryResponse, QueryStatus, ResultMetadata, SymbolId, SymbolRecord, WorkspacePath,
    document_from_bytes,
};

use super::{WorkspaceContext, print_response};

pub(super) fn fan_out(
    mut targets: Vec<String>,
    mut query: impl FnMut(String) -> Result<QueryStatus>,
) -> Result<QueryStatus> {
    targets.sort();
    targets.dedup();
    let mut status = QueryStatus::NoResult;
    for target in targets {
        let next = query(target)?;
        if next == QueryStatus::Ok {
            status = QueryStatus::Ok;
        }
    }
    Ok(status)
}

pub(super) fn select_symbols(
    context: &WorkspaceContext,
    applies: String,
    kotlin_class: bool,
    java_class: bool,
    component: Option<String>,
    qualified_prefix: Option<String>,
    human_output: bool,
) -> Result<QueryStatus> {
    let workspace = context.path();
    use kide_core::selector::{
        LanguageView, Selector, SelectorPredicate, SelectorState, records, select,
    };
    let store = IndexStore::open(IndexStore::default_path(workspace))?;
    let applied_symbol = match store.symbols_with_qualified_name(&applies)?.as_slice() {
        [symbol] => symbol.clone(),
        [] => SymbolId::new(applies),
        candidates => bail!(
            "applied symbol name is ambiguous: {} candidates",
            candidates.len()
        ),
    };
    let mut selector = Selector {
        views: match (kotlin_class, java_class) {
            (true, false) => vec![LanguageView::KotlinClass],
            (false, true) => vec![LanguageView::JavaClass],
            (false, false) => Vec::new(),
            (true, true) => unreachable!("clap rejects conflicting language views"),
        },
        predicates: vec![SelectorPredicate::AppliedSymbol(applied_symbol)],
    };
    if let Some(component) = component {
        selector
            .predicates
            .push(SelectorPredicate::Component(kide_core::ComponentId::new(
                component,
            )));
    }
    if let Some(prefix) = qualified_prefix {
        selector
            .predicates
            .push(SelectorPredicate::QualifiedNamePrefix(prefix));
    }
    let result = select(&store, &selector)?;
    if human_output {
        print_short_symbols(&store, workspace, &result.symbols)?;
    } else {
        for record in records(&result) {
            println!("{}", serde_json::to_string(&record)?);
        }
    }
    Ok(match result.state {
        SelectorState::Complete => QueryStatus::Ok,
        SelectorState::Partial => QueryStatus::Stale,
        SelectorState::NoResult => QueryStatus::NoResult,
    })
}

pub(super) fn print_short_symbols(
    store: &IndexStore,
    workspace: &Path,
    symbols: &[kide_core::SymbolRecord],
) -> Result<()> {
    for symbol in symbols {
        print_short_symbol(store, workspace, symbol, None)?;
    }
    Ok(())
}

fn print_short_symbol(
    store: &IndexStore,
    workspace: &Path,
    symbol: &kide_core::SymbolRecord,
    label: Option<&str>,
) -> Result<()> {
    let name = symbol.qualified_name.as_deref().unwrap_or(&symbol.name);
    let location = short_source_location(
        store,
        workspace,
        &symbol.declaration.source_unit,
        symbol.name_range.bytes.start,
    )?;
    let prefix = label.map_or(String::new(), |label| format!("{label} "));
    println!(
        "{prefix}{} {name} {location}",
        format!("{:?}", symbol.kind).to_lowercase()
    );
    Ok(())
}

pub(super) fn definition(
    context: &WorkspaceContext,
    value: String,
    human_output: bool,
) -> Result<QueryStatus> {
    let workspace = context.path();
    let store = IndexStore::open(IndexStore::default_path(workspace))?;
    let (status, result, problems) = match resolve_target(&store, workspace, &value)? {
        TargetResolution::Symbol(id) => match store.symbol(&id)? {
            Some(symbol) => (
                QueryStatus::Ok,
                Some(QueryPayload::Definition { symbol }),
                Vec::new(),
            ),
            None => (
                QueryStatus::NoResult,
                None,
                vec![QueryProblem {
                    code: "resolved_symbol_missing".to_owned(),
                    message: "location resolved to a symbol absent from the index".to_owned(),
                    retryable: false,
                }],
            ),
        },
        TargetResolution::NoResult => (QueryStatus::NoResult, None, Vec::new()),
        TargetResolution::Stale => (
            QueryStatus::Stale,
            None,
            vec![QueryProblem {
                code: "stale_source_snapshot".to_owned(),
                message: "the source file changed after it was indexed; run kide index".to_owned(),
                retryable: true,
            }],
        ),
        TargetResolution::Ambiguous(candidates) => (
            QueryStatus::Ambiguous,
            None,
            vec![QueryProblem {
                code: "ambiguous_location".to_owned(),
                message: format!("location resolves to {} symbols", candidates.len()),
                retryable: false,
            }],
        ),
    };
    if human_output {
        match result {
            Some(QueryPayload::Definition { symbol }) => {
                print_short_symbol(&store, workspace, &symbol, Some("definition"))?;
            }
            None => println!("no definition"),
            _ => unreachable!("definition only returns definition payloads"),
        }
        return Ok(status);
    }
    print_response(&QueryResponse {
        schema_version: CANONICAL_SCHEMA_VERSION,
        status,
        result,
        metadata: ResultMetadata::empty(),
        problems,
    })?;
    Ok(status)
}

pub(super) fn references(
    context: &WorkspaceContext,
    value: String,
    short: bool,
) -> Result<QueryStatus> {
    let workspace = context.path();
    let store = IndexStore::open(IndexStore::default_path(workspace))?;
    let (status, result, problems) = match resolve_target(&store, workspace, &value)? {
        TargetResolution::Symbol(symbol) => {
            let mut references = store
                .references_to(&symbol)?
                .into_iter()
                .map(|edge| edge.source)
                .collect::<Vec<_>>();
            if let Some(symbol_record) = store.symbol(&symbol)?
                && matches!(
                    symbol_record.kind,
                    kide_core::SymbolKind::Class
                        | kide_core::SymbolKind::Interface
                        | kide_core::SymbolKind::Object
                        | kide_core::SymbolKind::Enum
                )
            {
                let constructors = store
                    .symbols_for_source(&symbol_record.declaration.source_unit)?
                    .into_iter()
                    .filter(|candidate| {
                        candidate.kind == kide_core::SymbolKind::Constructor
                            && (candidate.owner.as_ref() == Some(&symbol)
                                || candidate.name_range == symbol_record.name_range)
                    });
                for constructor in constructors {
                    references.extend(
                        store
                            .calls_to(&constructor.id)?
                            .into_iter()
                            .map(|edge| edge.source),
                    );
                }
            }
            references.sort_by_key(|reference| {
                (
                    reference.range.source_unit.as_str().to_owned(),
                    reference.range.bytes.start,
                    reference.range.bytes.end,
                    format!("{:?}", reference.kind),
                    reference
                        .target
                        .as_ref()
                        .map(SymbolId::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                )
            });
            references.dedup();
            if short {
                if references.is_empty() {
                    println!("no references");
                    return Ok(QueryStatus::NoResult);
                }
                print_short_references(&store, workspace, &references)?;
                return Ok(QueryStatus::Ok);
            }
            query_result(QueryPayload::Refs { references })
        }
        resolution => target_problem(resolution),
    };
    print_query_response(status, result, problems)
}

fn print_short_references(
    store: &IndexStore,
    workspace: &Path,
    references: &[kide_core::SourceOccurrence],
) -> Result<()> {
    for reference in references {
        let Some(source) = store.source_unit(&reference.range.source_unit)? else {
            bail!("reference source unit is absent from the index");
        };
        let path = source.path.as_str();
        let text = std::fs::read_to_string(workspace.join(path)).ok();
        let context = store
            .occurrences_at(&source.id, reference.range.bytes.start)?
            .into_iter()
            .filter(|candidate| {
                candidate.kind == kide_core::OccurrenceKind::TypeReference
                    && candidate.range.bytes.start <= reference.range.bytes.start
                    && reference.range.bytes.end <= candidate.range.bytes.end
            })
            .max_by_key(|candidate| candidate.range.bytes.end - candidate.range.bytes.start)
            .map(|candidate| candidate.range.bytes)
            .unwrap_or(reference.range.bytes);
        let snippet = text
            .as_deref()
            .and_then(|text| {
                generic_type_context(text, reference.range.bytes.start, reference.range.bytes.end)
                    .or_else(|| source_snippet(text, context.start, context.end))
            })
            .unwrap_or_else(|| "<unavailable source>".to_owned());
        println!(
            "{snippet} {}",
            short_source_location(store, workspace, &source.id, reference.range.bytes.start)?
        );
    }
    Ok(())
}

fn source_snippet(text: &str, start: u64, end: u64) -> Option<String> {
    let start = usize::try_from(start).ok()?;
    let end = usize::try_from(end).ok()?;
    if start > end
        || end > text.len()
        || !text.is_char_boundary(start)
        || !text.is_char_boundary(end)
    {
        return None;
    }
    Some(
        text[start..end]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn generic_type_context(text: &str, start: u64, end: u64) -> Option<String> {
    let start = usize::try_from(start).ok()?;
    let end = usize::try_from(end).ok()?;
    let open = text[..start].rfind('<')?;
    let close = end + text[end..].find('>')? + 1;
    let name_start = text[..open]
        .char_indices()
        .rev()
        .take_while(|(_, character)| {
            character.is_alphanumeric() || *character == '.' || *character == '_'
        })
        .last()
        .map_or(open, |(offset, _)| offset);
    source_snippet(text, name_start as u64, close as u64)
}

fn short_source_location(
    store: &IndexStore,
    workspace: &Path,
    source_unit: &kide_core::SourceUnitId,
    byte_offset: u64,
) -> Result<String> {
    let Some(source) = store.source_unit(source_unit)? else {
        bail!("source unit is absent from the index");
    };
    let path = source.path.as_str();
    let location = std::fs::read_to_string(workspace.join(path))
        .ok()
        .and_then(|text| byte_to_location(&text, byte_offset));
    Ok(match location {
        Some((line, column)) => format!("{path}:{line}:{column}"),
        None => format!("{path}:byte:{byte_offset}"),
    })
}

pub(super) fn byte_to_location(text: &str, byte_offset: u64) -> Option<(usize, usize)> {
    let byte_offset = usize::try_from(byte_offset).ok()?;
    if byte_offset > text.len() || !text.is_char_boundary(byte_offset) {
        return None;
    }
    let prefix = &text[..byte_offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let line_start = prefix.rfind('\n').map_or(0, |offset| offset + 1);
    let column = text[line_start..byte_offset].chars().count() + 1;
    Some((line, column))
}

pub(super) fn callers(
    context: &WorkspaceContext,
    value: String,
    human_output: bool,
) -> Result<QueryStatus> {
    let workspace = context.path();
    let store = IndexStore::open(IndexStore::default_path(workspace))?;
    let (status, result, problems) = match resolve_target(&store, workspace, &value)? {
        TargetResolution::Symbol(symbol) => {
            let calls = store
                .calls_to(&symbol)?
                .into_iter()
                .map(|edge| edge.source)
                .collect::<Vec<_>>();
            if human_output {
                if calls.is_empty() {
                    println!("no callers");
                    return Ok(QueryStatus::NoResult);
                }
                print_short_references(&store, workspace, &calls)?;
                return Ok(QueryStatus::Ok);
            }
            query_result(QueryPayload::Callers { calls })
        }
        resolution => target_problem(resolution),
    };
    print_query_response(status, result, problems)
}

pub(super) fn implementations(
    context: &WorkspaceContext,
    value: String,
    transitive: bool,
    human_output: bool,
) -> Result<QueryStatus> {
    let workspace = context.path();
    let store = IndexStore::open(IndexStore::default_path(workspace))?;
    let (status, result, problems) = match resolve_target(&store, workspace, &value)? {
        TargetResolution::Symbol(symbol) => {
            let edges = if transitive {
                store.implementations_of_transitive(&symbol)?
            } else {
                store.implementations_of(&symbol)?
            };
            let mut symbols = edges
                .into_iter()
                .map(|edge| store.symbol(&edge.subtype))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            if symbols.is_empty() && !transitive {
                symbols = cached_dependency_implementations(
                    &store,
                    context.artifact_cache_root(),
                    &symbol,
                )?;
            }
            if human_output {
                if symbols.is_empty() {
                    println!("no implementations");
                    return Ok(QueryStatus::NoResult);
                }
                print_short_symbols(&store, workspace, &symbols)?;
                return Ok(QueryStatus::Ok);
            }
            query_result(QueryPayload::Implementations { symbols })
        }
        resolution => target_problem(resolution),
    };
    print_query_response(status, result, problems)
}

pub(super) fn cached_dependency_implementations(
    store: &IndexStore,
    cache_root: &Path,
    supertype: &SymbolId,
) -> Result<Vec<SymbolRecord>> {
    let cache = ArtifactBlobCache::open(cache_root)?;
    let mut symbols = Vec::new();
    for artifact in store.artifact_candidates_with_symbol(supertype)? {
        symbols.extend(kide_core::artifact_query::direct_implementations(
            &cache, &artifact, supertype,
        )?);
    }
    symbols.sort_by_key(|symbol| symbol.id.as_str().to_owned());
    symbols.dedup_by(|left, right| left.id == right.id);
    Ok(symbols)
}

pub(super) fn type_at(
    context: &WorkspaceContext,
    value: String,
    human_output: bool,
) -> Result<QueryStatus> {
    let workspace = context.path();
    let location = parse_location(&value)?;
    let source_text = std::fs::read_to_string(workspace.join(location.path.as_str()))?;
    let store = IndexStore::open(IndexStore::default_path(workspace))?;
    let units = store.source_units_at_path(&location.path)?;
    let [source_unit] = units.as_slice() else {
        return Err(anyhow::anyhow!("location path is not uniquely indexed"));
    };
    let offset = kide_core::query_resolver::byte_offset(&source_text, location.position)?;
    let occurrence = store
        .occurrences_at(&source_unit.id, offset)?
        .into_iter()
        .find(|occurrence| occurrence.type_id.is_some());
    let (status, result) = match occurrence {
        Some(occurrence) => match occurrence.type_id.as_ref() {
            Some(type_id) => match store
                .types_for(&source_unit.id)?
                .into_iter()
                .find(|ty| &ty.id == type_id)
            {
                Some(ty) => (
                    QueryStatus::Ok,
                    Some(QueryPayload::TypeAt { occurrence, ty }),
                ),
                None => (QueryStatus::NoResult, None),
            },
            None => (QueryStatus::NoResult, None),
        },
        None => (QueryStatus::NoResult, None),
    };
    if human_output {
        match result {
            Some(QueryPayload::TypeAt { ty, .. }) => println!("{}", ty.display),
            None => println!("no type"),
            _ => unreachable!("type-at only returns type payloads"),
        }
        return Ok(status);
    }
    print_query_response(status, result, Vec::new())
}

#[derive(Debug)]
pub(super) enum TargetResolution {
    Symbol(SymbolId),
    NoResult,
    Stale,
    Ambiguous(Vec<SymbolId>),
}

fn resolve_target(store: &IndexStore, workspace: &Path, value: &str) -> Result<TargetResolution> {
    if value.starts_with("jvm:sha256:")
        || value.starts_with("kotlin:")
        || value.starts_with("java:")
    {
        let symbol = SymbolId::new(value);
        return Ok(
            if store.symbol(&symbol)?.is_some()
                || !store.artifact_candidates_with_symbol(&symbol)?.is_empty()
            {
                TargetResolution::Symbol(symbol)
            } else {
                TargetResolution::NoResult
            },
        );
    }
    if let Ok(location) = parse_location(value) {
        let source_text = std::fs::read_to_string(workspace.join(location.path.as_str()))?;
        let units = store.source_units_at_path(&location.path)?;
        if let [unit] = units.as_slice() {
            let current =
                document_from_bytes(location.path.clone(), source_text.as_bytes().to_vec())
                    .map_err(|_| {
                        anyhow::anyhow!("location source is not an eligible UTF-8 text file")
                    })?;
            if current.fingerprint != unit.content {
                return Ok(TargetResolution::Stale);
            }
        }
        return Ok(
            match kide_core::query_resolver::resolve_location(store, &location, &source_text)? {
                kide_core::query_resolver::LocationResolution::Symbol(symbol) => {
                    TargetResolution::Symbol(symbol)
                }
                kide_core::query_resolver::LocationResolution::NoResult => {
                    TargetResolution::NoResult
                }
                kide_core::query_resolver::LocationResolution::Ambiguous { candidates } => {
                    TargetResolution::Ambiguous(candidates)
                }
            },
        );
    }
    let symbols = store.symbols_named(value)?;
    let candidates = symbols
        .into_iter()
        .map(|symbol| symbol.id)
        .collect::<Vec<_>>();
    Ok(match candidates.as_slice() {
        [] => TargetResolution::NoResult,
        [symbol] => TargetResolution::Symbol(symbol.clone()),
        _ => TargetResolution::Ambiguous(candidates),
    })
}

fn query_result(payload: QueryPayload) -> (QueryStatus, Option<QueryPayload>, Vec<QueryProblem>) {
    match &payload {
        QueryPayload::Refs { references } if references.is_empty() => {
            (QueryStatus::NoResult, None, Vec::new())
        }
        QueryPayload::Callers { calls } if calls.is_empty() => {
            (QueryStatus::NoResult, None, Vec::new())
        }
        QueryPayload::Implementations { symbols } if symbols.is_empty() => {
            (QueryStatus::NoResult, None, Vec::new())
        }
        _ => (QueryStatus::Ok, Some(payload), Vec::new()),
    }
}

pub(super) fn target_problem(
    resolution: TargetResolution,
) -> (QueryStatus, Option<QueryPayload>, Vec<QueryProblem>) {
    match resolution {
        TargetResolution::NoResult => (QueryStatus::NoResult, None, Vec::new()),
        TargetResolution::Stale => (
            QueryStatus::Stale,
            None,
            vec![QueryProblem {
                code: "stale_source_snapshot".to_owned(),
                message: "the source file changed after it was indexed; run kide index".to_owned(),
                retryable: true,
            }],
        ),
        TargetResolution::Ambiguous(candidates) => (
            QueryStatus::Ambiguous,
            None,
            vec![QueryProblem {
                code: "ambiguous_target".to_owned(),
                message: format!("target resolves to {} symbols", candidates.len()),
                retryable: false,
            }],
        ),
        TargetResolution::Symbol(_) => unreachable!("resolved symbol is handled by caller"),
    }
}

pub(super) fn print_query_response(
    status: QueryStatus,
    result: Option<QueryPayload>,
    problems: Vec<QueryProblem>,
) -> Result<QueryStatus> {
    print_response(&QueryResponse {
        schema_version: CANONICAL_SCHEMA_VERSION,
        status,
        result,
        metadata: ResultMetadata::empty(),
        problems,
    })?;
    Ok(status)
}

fn parse_location(value: &str) -> Result<kide_core::Location> {
    let mut parts = value.rsplitn(3, ':');
    let column = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("location must be PATH:LINE:COLUMN"))?
        .parse()?;
    let line = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("location must be PATH:LINE:COLUMN"))?
        .parse()?;
    let path = parts
        .next()
        .filter(|path| !path.is_empty())
        .ok_or_else(|| anyhow::anyhow!("location must be PATH:LINE:COLUMN"))?;
    Ok(kide_core::Location {
        path: WorkspacePath::new(path),
        position: kide_core::TextPosition { line, column },
    })
}
