use std::{
    ffi::OsString,
    io::{IsTerminal, Read},
    path::{Path, PathBuf},
    time::Duration,
};

use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::{ArgAction, Parser, Subcommand};
use kide_core::{
    ArtifactBlobCache, BuildSystem, CANONICAL_SCHEMA_VERSION, IndexStore, QueryPayload,
    QueryProblem, QueryResponse, QueryStatus, ResultMetadata, SymbolId, WorkerCapability,
    WorkerInstallation, WorkerLaunch, WorkerRegistry, WorkspacePath, collect_workspace_text,
    discover_workspace, index_batch_with_artifact_cache, index_selected_batches,
};

/// Headless, persistent semantic code platform.
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Show operational progress. Repeat for more detail.
    #[arg(short, long, action = ArgAction::Count, global = true)]
    verbose: u8,

    /// Root of a previously indexed workspace for query commands.
    #[arg(long, global = true, default_value = ".")]
    workspace: PathBuf,

    /// Emit the versioned JSON response even when stdout is a terminal.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Discover a workspace and persist its semantic index.
    Index {
        path: PathBuf,
        /// Reanalyze workspace source units even when their fingerprints match.
        #[arg(long)]
        force: bool,
    },
    /// Show persisted index health, freshness, and worker provenance.
    Status,
    /// Find symbols by name or qualified query.
    Symbols {
        query: String,
        /// Print compact human-readable declaration locations.
        #[arg(long)]
        short: bool,
    },
    /// Select declarations through resolved semantic predicates.
    Select {
        /// Resolved SymbolId applied to the declarations to select.
        #[arg(long)]
        applies: String,
        /// Apply the reusable Kotlin class view.
        #[arg(long)]
        kotlin_class: bool,
        #[arg(long)]
        component: Option<String>,
        #[arg(long = "qualified-prefix")]
        qualified_prefix: Option<String>,
    },
    /// Resolve a declaration from a location, stable SymbolId, exact name, or stdin.
    Definition {
        /// A location, stable SymbolId, or exact symbol name. When omitted,
        /// read one `kide symbols` response from stdin.
        target: Option<String>,
    },
    /// Find exact semantic references for a location or symbol query.
    #[command(alias = "ref")]
    Refs {
        /// A location, stable SymbolId, or exact symbol name.
        target: Option<String>,
        /// Print source locations as one `path:line:column` record per line.
        #[arg(long)]
        short: bool,
    },
    /// Find implementations for a location or symbol query.
    Implementations { target: Option<String> },
    /// Find resolved callers for a location or symbol query.
    Callers { target: Option<String> },
    /// Resolve the type at PATH:LINE:COLUMN.
    #[command(name = "type-at")]
    TypeAt { location: String },
}

fn main() -> ExitCode {
    match run() {
        Ok(status) => exit_code(status),
        Err(error) => {
            if std::io::stdout().is_terminal() {
                eprintln!("error: {error}");
                return ExitCode::from(4);
            }
            let response = QueryResponse {
                schema_version: CANONICAL_SCHEMA_VERSION,
                status: QueryStatus::Failed,
                result: None,
                metadata: ResultMetadata::empty(),
                problems: vec![QueryProblem {
                    code: "internal_error".to_owned(),
                    message: error.to_string(),
                    retryable: false,
                }],
            };

            let _ = print_response(&response);
            ExitCode::from(4)
        }
    }
}

fn run() -> Result<QueryStatus> {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    let human_output = !cli.json && std::io::stdout().is_terminal();

    match cli.command {
        Command::Index { path, force } => index(path, cli.verbose, force),
        Command::Status => pending("status", String::new()),
        Command::Symbols { query, short } => symbols(&cli.workspace, query, short || human_output),
        Command::Select {
            applies,
            kotlin_class,
            component,
            qualified_prefix,
        } => select_symbols(
            &cli.workspace,
            applies,
            kotlin_class,
            component,
            qualified_prefix,
            human_output,
        ),
        Command::Definition { target } => definition(
            &cli.workspace,
            target_from_argument_or_stdin(target)?,
            human_output,
        ),
        Command::Refs { target, short } => {
            fan_out(targets_from_argument_or_stdin(target)?, |target| {
                references(&cli.workspace, target, short || human_output)
            })
        }
        Command::Implementations { target } => {
            fan_out(targets_from_argument_or_stdin(target)?, |target| {
                implementations(&cli.workspace, target, human_output)
            })
        }
        Command::Callers { target } => fan_out(targets_from_argument_or_stdin(target)?, |target| {
            callers(&cli.workspace, target, human_output)
        }),
        Command::TypeAt { location } => type_at(&cli.workspace, location, human_output),
    }
}

fn target_from_argument_or_stdin(target: Option<String>) -> Result<String> {
    let targets = targets_from_argument_or_stdin(target)?;
    match targets.as_slice() {
        [target] => Ok(target.clone()),
        _ => bail!(
            "this command requires exactly one target, but the pipe contains {}",
            targets.len()
        ),
    }
}

fn targets_from_argument_or_stdin(target: Option<String>) -> Result<Vec<String>> {
    if let Some(target) = target {
        return Ok(vec![target]);
    }
    if std::io::stdin().is_terminal() {
        bail!(
            "a target is required, or pipe one `kide symbols`/`kide definition` response to stdin"
        );
    }

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(target_from_pipe_text)
        .collect()
}

fn fan_out(
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

fn target_from_pipe_text(input: &str) -> Result<String> {
    if let Ok(record) = serde_json::from_str::<kide_core::SelectorRecord>(input) {
        return Ok(record.symbol.id.as_str().to_owned());
    }
    let response: QueryResponse = serde_json::from_str(input).map_err(|error| {
        anyhow::anyhow!("--stdin expects one kide JSON response or selector JSONL record: {error}")
    })?;
    target_from_pipe_response(response)
}

fn target_from_pipe_response(response: QueryResponse) -> Result<String> {
    let result = response
        .result
        .ok_or_else(|| anyhow::anyhow!("--stdin response contains no result"))?;
    match result {
        QueryPayload::Definition { symbol } => Ok(symbol.id.as_str().to_owned()),
        QueryPayload::Symbols { symbols } => {
            target_from_symbols(symbols.into_iter().map(|symbol| symbol.id).collect())
        }
        _ => bail!("--stdin accepts only `kide symbols` or `kide definition` output"),
    }
}

fn target_from_symbols(symbols: Vec<SymbolId>) -> Result<String> {
    match symbols.as_slice() {
        [symbol] => Ok(symbol.as_str().to_owned()),
        _ => bail!(
            "--stdin needs exactly one symbol, but the upstream query returned {}",
            symbols.len()
        ),
    }
}

fn symbols(workspace: &Path, query: String, short: bool) -> Result<QueryStatus> {
    let store = IndexStore::open(IndexStore::default_path(workspace))?;
    let symbols = store.symbols_named(&query)?;
    if short && !symbols.is_empty() {
        print_short_symbols(&store, workspace, &symbols)?;
        return Ok(QueryStatus::Ok);
    }
    let (status, result) = if symbols.is_empty() {
        (QueryStatus::NoResult, None)
    } else {
        (QueryStatus::Ok, Some(QueryPayload::Symbols { symbols }))
    };
    let response = QueryResponse {
        schema_version: CANONICAL_SCHEMA_VERSION,
        status,
        result,
        metadata: ResultMetadata::empty(),
        problems: Vec::new(),
    };
    print_response(&response)?;
    Ok(status)
}

fn select_symbols(
    workspace: &Path,
    applies: String,
    kotlin_class: bool,
    component: Option<String>,
    qualified_prefix: Option<String>,
    human_output: bool,
) -> Result<QueryStatus> {
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
        views: kotlin_class
            .then_some(LanguageView::KotlinClass)
            .into_iter()
            .collect(),
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

fn print_short_symbols(
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

fn definition(workspace: &Path, value: String, human_output: bool) -> Result<QueryStatus> {
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

fn references(workspace: &Path, value: String, short: bool) -> Result<QueryStatus> {
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

fn byte_to_location(text: &str, byte_offset: u64) -> Option<(usize, usize)> {
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

fn callers(workspace: &Path, value: String, human_output: bool) -> Result<QueryStatus> {
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

fn implementations(workspace: &Path, value: String, human_output: bool) -> Result<QueryStatus> {
    let store = IndexStore::open(IndexStore::default_path(workspace))?;
    let (status, result, problems) = match resolve_target(&store, workspace, &value)? {
        TargetResolution::Symbol(symbol) => {
            let symbols = store
                .implementations_of(&symbol)?
                .into_iter()
                .map(|edge| store.symbol(&edge.subtype))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
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

fn type_at(workspace: &Path, value: String, human_output: bool) -> Result<QueryStatus> {
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
enum TargetResolution {
    Symbol(SymbolId),
    NoResult,
    Ambiguous(Vec<SymbolId>),
}

fn resolve_target(store: &IndexStore, workspace: &Path, value: &str) -> Result<TargetResolution> {
    if value.starts_with("jvm:sha256:") || value.starts_with("kotlin:") {
        return Ok(if store.symbol(&SymbolId::new(value))?.is_some() {
            TargetResolution::Symbol(SymbolId::new(value))
        } else {
            TargetResolution::NoResult
        });
    }
    if let Ok(location) = parse_location(value) {
        let source_text = std::fs::read_to_string(workspace.join(location.path.as_str()))?;
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

fn target_problem(
    resolution: TargetResolution,
) -> (QueryStatus, Option<QueryPayload>, Vec<QueryProblem>) {
    match resolution {
        TargetResolution::NoResult => (QueryStatus::NoResult, None, Vec::new()),
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

fn print_query_response(
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

fn index(path: PathBuf, verbosity: u8, force: bool) -> Result<QueryStatus> {
    tracing::debug!(target: "kide::cli", workspace = %path.display(), "discovering workspace");
    let discovery = discover_workspace(&path)?;
    tracing::debug!(target: "kide::cli", "opening index");
    let mut store = IndexStore::open(IndexStore::default_path(&discovery.root))?;
    let sources = discovery.source_units;
    if force {
        tracing::info!(target: "kide::cli", sources = sources.len(), "forcing source reanalysis");
        for source in &sources {
            store.remove_snapshot(&source.id)?;
        }
    }
    let artifact_cache_root = std::env::var_os("KIDE_ARTIFACT_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| discovery.root.join(".kide/artifact-cache"));
    let artifact_cache = ArtifactBlobCache::open(artifact_cache_root)?;
    let staging = discovery.root.join(".kide/staging");
    std::fs::create_dir_all(&staging)?;
    let registry = WorkerRegistry::new(vec![kotlin_worker_installation(
        &discovery.root,
        verbosity,
    )?]);
    let selection = registry.select(
        &discovery.manifest,
        sources.clone(),
        &[WorkerCapability::FileAnalysisSnapshot],
    )?;
    if !selection.unsupported.is_empty() {
        let unsupported = selection
            .unsupported
            .iter()
            .map(|batch| serde_json::json!({
                "component": batch.component.as_str(),
                "language": batch.language,
                "build_system": batch.build_system,
                "source_units": batch.source_units.iter().map(|source| source.path.as_str()).collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>();
        bail!(
            "unsupported_worker_batches={}",
            serde_json::to_string(&unsupported)?
        );
    }
    let run = if selection.batches.len() == 1 {
        // Retain the cache-aware dependency catalog path for the common
        // single-backend workspace. Mixed workspaces use the global planner so
        // one batch cannot invalidate another language's source snapshots.
        index_batch_with_artifact_cache(
            &mut store,
            &discovery.manifest,
            &sources,
            selection.batches[0].worker.installation.launch.clone(),
            &artifact_cache,
            &staging,
        )?
    } else {
        index_selected_batches(&mut store, &discovery.manifest, &sources, &selection)?
    };
    let text_inventory = collect_workspace_text(&discovery.root)?;
    store.sync_text_documents(&text_inventory.documents)?;
    tracing::info!(target: "kide::cli", analyzed = run.analyzed, dependency_analyzed = run.dependency_analyzed, "index complete");
    println!(
        "{}",
        serde_json::json!({
            "schema_version": CANONICAL_SCHEMA_VERSION,
            "status": "ok",
            "workspace": discovery.root,
            "reused": run.reused,
            "analyzed": run.analyzed,
        "removed": run.removed,
        "worker_starts": run.worker_starts,
        "dependency_analyzed": run.dependency_analyzed,
        "dependency_reused": run.dependency_reused,
        "text_documents": text_inventory.documents.len(),
        "text_skipped": text_inventory.skipped.len(),
        })
    );
    Ok(QueryStatus::Ok)
}

fn init_logging(verbosity: u8) {
    let fallback = match verbosity {
        0 => "kide=warn",
        1 => "kide=info",
        _ => "kide=debug",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(fallback));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .compact()
        .init();
}

fn kotlin_worker_installation(workspace: &Path, verbosity: u8) -> Result<WorkerInstallation> {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let worker = repository.join("workers/kotlin-jvm");
    let executable = worker.join("build/install/kide-kotlin-jvm-worker/bin/kide-kotlin-jvm-worker");
    if !executable.is_file() {
        bail!(
            "Kotlin worker distribution is missing at {}; run `make build` before indexing",
            executable.display()
        );
    }
    let mut launch = WorkerLaunch::new(executable);
    launch.args = vec![OsString::from("--serve")];
    launch.environment.insert(
        OsString::from("KIDE_WORKSPACE_ROOT"),
        workspace.as_os_str().to_os_string(),
    );
    launch.environment.insert(
        OsString::from("KIDE_WORKER_LOG_LEVEL"),
        OsString::from(match verbosity {
            0 => "warn",
            1 => "info",
            _ => "debug",
        }),
    );
    // A cold K2 batch over a realistic multi-module workspace can exceed the
    // control-plane default while still making progress.
    launch.request_timeout = Duration::from_secs(5 * 60);
    Ok(WorkerInstallation {
        name: "kotlin-jvm".to_owned(),
        launch,
        build_systems: vec![BuildSystem::Gradle, BuildSystem::Filesystem],
    })
}

fn pending(command: &str, argument: String) -> Result<QueryStatus> {
    let response = QueryResponse {
        schema_version: CANONICAL_SCHEMA_VERSION,
        status: QueryStatus::Unsupported,
        result: None,
        metadata: ResultMetadata::empty(),
        problems: vec![QueryProblem {
            code: "command_not_implemented".to_owned(),
            message: format!("{command} is defined but not implemented; argument: {argument}"),
            retryable: false,
        }],
    };

    print_response(&response)?;
    Ok(QueryStatus::Unsupported)
}

fn print_response(response: &QueryResponse) -> Result<()> {
    println!("{}", serde_json::to_string(response)?);
    Ok(())
}

fn exit_code(status: QueryStatus) -> ExitCode {
    match status {
        QueryStatus::Ok => ExitCode::SUCCESS,
        QueryStatus::NoResult | QueryStatus::Ambiguous => ExitCode::from(1),
        QueryStatus::InvalidRequest => ExitCode::from(2),
        QueryStatus::Stale | QueryStatus::Unsupported => ExitCode::from(3),
        QueryStatus::Failed => ExitCode::from(4),
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::{
        Cli, ExitCode, byte_to_location, exit_code, fan_out, target_from_pipe_text,
        target_from_symbols,
    };

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn exit_codes_match_the_v1_contract() {
        assert_eq!(exit_code(kide_core::QueryStatus::Ok), ExitCode::SUCCESS);
        assert_eq!(
            exit_code(kide_core::QueryStatus::Ambiguous),
            ExitCode::from(1)
        );
        assert_eq!(
            exit_code(kide_core::QueryStatus::InvalidRequest),
            ExitCode::from(2)
        );
        assert_eq!(
            exit_code(kide_core::QueryStatus::Unsupported),
            ExitCode::from(3)
        );
        assert_eq!(exit_code(kide_core::QueryStatus::Failed), ExitCode::from(4));
    }

    #[test]
    fn pipe_input_uses_the_stable_symbol_id() {
        assert_eq!(
            target_from_symbols(vec![kide_core::SymbolId::new("kotlin:controller")]).unwrap(),
            "kotlin:controller"
        );
    }

    #[test]
    fn pipe_input_rejects_ambiguous_symbol_results() {
        assert!(
            target_from_symbols(vec![
                kide_core::SymbolId::new("kotlin:first"),
                kide_core::SymbolId::new("kotlin:second"),
            ])
            .is_err()
        );
    }

    #[test]
    fn selector_jsonl_record_is_a_navigation_target() {
        let record = kide_core::SelectorRecord {
            symbol: test_symbol(),
            metadata: kide_core::ResultMetadata::empty(),
        };
        assert_eq!(
            target_from_pipe_text(&serde_json::to_string(&record).unwrap()).unwrap(),
            "kotlin:controller"
        );
    }

    #[test]
    fn fan_out_deduplicates_selector_targets_and_reports_success() {
        let mut seen = Vec::new();
        assert_eq!(
            fan_out(vec!["b".into(), "a".into(), "a".into()], |target| {
                seen.push(target);
                Ok(kide_core::QueryStatus::Ok)
            })
            .unwrap(),
            kide_core::QueryStatus::Ok
        );
        assert_eq!(seen, vec!["a", "b"]);
    }

    fn test_symbol() -> kide_core::SymbolRecord {
        kide_core::SymbolRecord {
            id: kide_core::SymbolId::new("kotlin:controller"),
            backend_key: kide_core::BackendKey {
                backend: "test".into(),
                schema_version: 1,
                value: "key".into(),
            },
            language: kide_core::Language::Kotlin,
            kind: kide_core::SymbolKind::Class,
            name: "Controller".into(),
            qualified_name: None,
            signature: None,
            component: kide_core::ComponentId::new("test"),
            declaration: kide_core::SourceRange {
                source_unit: kide_core::SourceUnitId::new("test"),
                bytes: kide_core::ByteRange { start: 0, end: 0 },
            },
            name_range: kide_core::SourceRange {
                source_unit: kide_core::SourceUnitId::new("test"),
                bytes: kide_core::ByteRange { start: 0, end: 0 },
            },
            owner: None,
            modifiers: vec![],
            applied_symbols: vec![],
            freshness: kide_core::Freshness::Fresh,
            completeness: kide_core::Completeness::Complete,
            provenance: kide_core::Provenance {
                backend: "test".into(),
                backend_version: "1".into(),
                protocol_version: kide_core::WORKER_PROTOCOL_VERSION,
                analysis_options: kide_core::Fingerprint::new("test"),
            },
        }
    }

    #[test]
    fn short_locations_use_one_based_unicode_scalar_coordinates() {
        assert_eq!(byte_to_location("a😀b\nnext", 5), Some((1, 3)));
        assert_eq!(byte_to_location("a😀b\nnext", 7), Some((2, 1)));
        assert_eq!(byte_to_location("a😀b", 2), None);
    }
}
