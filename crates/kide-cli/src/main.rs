use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::{ArgAction, Parser, Subcommand};
use kide_core::{
    ArtifactBlobCache, CANONICAL_SCHEMA_VERSION, IndexStore, Language, QueryPayload, QueryProblem,
    QueryResponse, QueryStatus, ResultMetadata, SymbolId, WorkerLaunch, WorkspacePath,
    discover_workspace, index_batch_with_artifact_cache,
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

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Discover a workspace and persist its semantic index.
    Index { path: PathBuf },
    /// Show persisted index health, freshness, and worker provenance.
    Status,
    /// Find symbols by name or qualified query.
    Symbols { query: String },
    /// Resolve the declaration at PATH:LINE:COLUMN.
    Definition { location: String },
    /// Find exact semantic references for a location or symbol query.
    Refs { target: String },
    /// Find implementations for a location or symbol query.
    Implementations { target: String },
    /// Find resolved callers for a location or symbol query.
    Callers { target: String },
    /// Resolve the type at PATH:LINE:COLUMN.
    #[command(name = "type-at")]
    TypeAt { location: String },
}

fn main() -> ExitCode {
    match run() {
        Ok(status) => exit_code(status),
        Err(error) => {
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

    match cli.command {
        Command::Index { path } => index(path, cli.verbose),
        Command::Status => pending("status", String::new()),
        Command::Symbols { query } => symbols(&cli.workspace, query),
        Command::Definition { location } => definition(&cli.workspace, location),
        Command::Refs { target } => references(&cli.workspace, target),
        Command::Implementations { target } => implementations(&cli.workspace, target),
        Command::Callers { target } => callers(&cli.workspace, target),
        Command::TypeAt { location } => type_at(&cli.workspace, location),
    }
}

fn symbols(workspace: &Path, query: String) -> Result<QueryStatus> {
    let store = IndexStore::open(IndexStore::default_path(workspace))?;
    let symbols = store.symbols_named(&query)?;
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

fn definition(workspace: &Path, value: String) -> Result<QueryStatus> {
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
    print_response(&QueryResponse {
        schema_version: CANONICAL_SCHEMA_VERSION,
        status,
        result,
        metadata: ResultMetadata::empty(),
        problems,
    })?;
    Ok(status)
}

fn references(workspace: &Path, value: String) -> Result<QueryStatus> {
    let store = IndexStore::open(IndexStore::default_path(workspace))?;
    let (status, result, problems) = match resolve_target(&store, workspace, &value)? {
        TargetResolution::Symbol(symbol) => {
            let references = store
                .references_to(&symbol)?
                .into_iter()
                .map(|edge| edge.source)
                .collect::<Vec<_>>();
            query_result(QueryPayload::Refs { references })
        }
        resolution => target_problem(resolution),
    };
    print_query_response(status, result, problems)
}

fn callers(workspace: &Path, value: String) -> Result<QueryStatus> {
    let store = IndexStore::open(IndexStore::default_path(workspace))?;
    let (status, result, problems) = match resolve_target(&store, workspace, &value)? {
        TargetResolution::Symbol(symbol) => {
            let calls = store
                .calls_to(&symbol)?
                .into_iter()
                .map(|edge| edge.source)
                .collect::<Vec<_>>();
            query_result(QueryPayload::Callers { calls })
        }
        resolution => target_problem(resolution),
    };
    print_query_response(status, result, problems)
}

fn implementations(workspace: &Path, value: String) -> Result<QueryStatus> {
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
            query_result(QueryPayload::Implementations { symbols })
        }
        resolution => target_problem(resolution),
    };
    print_query_response(status, result, problems)
}

fn type_at(workspace: &Path, value: String) -> Result<QueryStatus> {
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

fn index(path: PathBuf, verbosity: u8) -> Result<QueryStatus> {
    tracing::debug!(target: "kide::cli", workspace = %path.display(), "discovering workspace");
    let discovery = discover_workspace(&path)?;
    tracing::debug!(target: "kide::cli", "opening index");
    let mut store = IndexStore::open(IndexStore::default_path(&discovery.root))?;
    let sources = discovery
        .source_units
        .into_iter()
        .filter(|source| source.language == Language::Kotlin)
        .collect::<Vec<_>>();
    let artifact_cache_root = std::env::var_os("KIDE_ARTIFACT_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| discovery.root.join(".kide/artifact-cache"));
    let artifact_cache = ArtifactBlobCache::open(artifact_cache_root)?;
    let staging = discovery.root.join(".kide/staging");
    std::fs::create_dir_all(&staging)?;
    let run = index_batch_with_artifact_cache(
        &mut store,
        &discovery.manifest,
        &sources,
        kotlin_worker_launch(&discovery.root, verbosity)?,
        &artifact_cache,
        &staging,
    )?;
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

fn kotlin_worker_launch(workspace: &Path, verbosity: u8) -> Result<WorkerLaunch> {
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
    Ok(launch)
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

    use super::{Cli, ExitCode, exit_code};

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
}
