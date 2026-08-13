use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

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

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Index { path } => pending("index", path.display().to_string()),
        Command::Status => pending("status", String::new()),
        Command::Symbols { query } => pending("symbols", query),
        Command::Definition { location } => pending("definition", location),
        Command::Refs { target } => pending("refs", target),
        Command::Implementations { target } => pending("implementations", target),
        Command::Callers { target } => pending("callers", target),
        Command::TypeAt { location } => pending("type-at", location),
    }
}

fn pending(command: &str, argument: String) -> Result<()> {
    let response = serde_json::json!({
        "command": command,
        "argument": argument,
        "index_format_version": kide_core::INDEX_FORMAT_VERSION,
        "status": "not_implemented"
    });

    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
