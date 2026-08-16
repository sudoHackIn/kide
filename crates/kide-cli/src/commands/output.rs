use std::process::ExitCode;

use anyhow::Result;
use kide_core::{QueryResponse, QueryStatus};

pub(crate) fn print_response(response: &QueryResponse) -> Result<()> {
    println!("{}", serde_json::to_string(response)?);
    Ok(())
}

pub(crate) fn exit_code(status: QueryStatus) -> ExitCode {
    match status {
        QueryStatus::Ok => ExitCode::SUCCESS,
        QueryStatus::NoResult | QueryStatus::Ambiguous => ExitCode::from(1),
        QueryStatus::InvalidRequest => ExitCode::from(2),
        QueryStatus::Stale | QueryStatus::Unsupported => ExitCode::from(3),
        QueryStatus::Failed => ExitCode::from(4),
    }
}
