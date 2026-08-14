use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use kide_core::{
    ArtifactBlobCache, CANONICAL_SCHEMA_VERSION, IndexStore, Language, QueryProblem, QueryResponse,
    QueryStatus, ResultMetadata, WorkerLaunch, discover_workspace, index_batch_with_artifact_cache,
};

/// Headless, persistent semantic code platform.
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Cli {
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

    match cli.command {
        Command::Index { path } => index(path),
        Command::Status => pending("status", String::new()),
        Command::Symbols { query } => pending("symbols", query),
        Command::Definition { location } => pending("definition", location),
        Command::Refs { target } => pending("refs", target),
        Command::Implementations { target } => pending("implementations", target),
        Command::Callers { target } => pending("callers", target),
        Command::TypeAt { location } => pending("type-at", location),
    }
}

fn index(path: PathBuf) -> Result<QueryStatus> {
    let discovery = discover_workspace(&path)?;
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
        kotlin_worker_launch(&discovery.root),
        &artifact_cache,
        &staging,
    )?;
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

fn kotlin_worker_launch(workspace: &Path) -> WorkerLaunch {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let worker = repository.join("workers/kotlin-jvm");
    let mut launch = WorkerLaunch::new(worker.join("gradlew"));
    launch.args = vec![
        OsString::from("-q"),
        OsString::from("--project-dir"),
        worker.into_os_string(),
        OsString::from("run"),
        OsString::from("--args=--serve"),
    ];
    launch.environment.insert(
        OsString::from("KIDE_WORKSPACE_ROOT"),
        workspace.as_os_str().to_os_string(),
    );
    launch
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
