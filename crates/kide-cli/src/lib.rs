//! Testable command library behind the tiny `kide` process entry point.

use std::{io::IsTerminal, path::PathBuf, process::ExitCode};

use anyhow::Result;
use clap::{ArgAction, Parser, Subcommand};
use kide_core::{
    CANONICAL_SCHEMA_VERSION, QueryProblem, QueryResponse, QueryStatus, ResultMetadata,
};

mod commands;

/// Headless, persistent semantic code platform.
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub(crate) struct Cli {
    #[arg(short, long, action = ArgAction::Count, global = true)]
    pub(crate) verbose: u8,
    #[arg(long, global = true, default_value = ".")]
    pub(crate) workspace: PathBuf,
    #[arg(long, global = true)]
    pub(crate) json: bool,
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    Index {
        path: PathBuf,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        warm_dependencies: bool,
    },
    Status,
    Text {
        query: String,
    },
    Symbols {
        query: String,
        #[arg(long)]
        short: bool,
    },
    Select {
        #[arg(long)]
        applies: String,
        #[arg(long)]
        kotlin_class: bool,
        #[arg(long, conflicts_with = "kotlin_class")]
        java_class: bool,
        #[arg(long)]
        component: Option<String>,
        #[arg(long = "qualified-prefix")]
        qualified_prefix: Option<String>,
    },
    Definition {
        target: Option<String>,
    },
    Refs {
        target: Option<String>,
        #[arg(long)]
        short: bool,
    },
    Implementations {
        target: Option<String>,
        #[arg(long)]
        transitive: bool,
    },
    Callers {
        target: Option<String>,
    },
    #[command(name = "type-at")]
    TypeAt {
        location: String,
    },
}

pub fn run_cli() -> ExitCode {
    match run() {
        Ok(status) => commands::exit_code(status),
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
            let _ = commands::print_response(&response);
            ExitCode::from(4)
        }
    }
}

fn run() -> Result<QueryStatus> {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    let human_output = !cli.json && std::io::stdout().is_terminal();
    commands::dispatch(cli, human_output)
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
