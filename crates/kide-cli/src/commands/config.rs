use std::path::Path;

use anyhow::Result;
use kide_core::{CANONICAL_SCHEMA_VERSION, QueryStatus, initialize_workspace_configuration};

/// Explicitly creates the workspace configuration. This is the only CLI
/// command that creates `.kide/config.toml`; `index` rejects a missing file so
/// automation does not gain an unexpected working-tree mutation.
pub(super) fn init(workspace: &Path) -> Result<QueryStatus> {
    let path = initialize_workspace_configuration(workspace)?;
    println!(
        "{}",
        serde_json::json!({
            "schema_version": CANONICAL_SCHEMA_VERSION,
            "status": "ok",
            "workspace": workspace,
            "configuration": path,
            "freshness_strategy": "fresh_only",
        })
    );
    Ok(QueryStatus::Ok)
}
