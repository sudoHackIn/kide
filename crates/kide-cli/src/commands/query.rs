use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;
use kide_core::{
    query_package::{PackageRegistry, PackageSource},
    project_query::ProjectQuery,
    selector::{records, SelectorResult, SelectorState},
    semantic_query::{execute, QueryResult},
    IndexStore, QueryStatus,
};

pub(super) fn run(workspace: &Path, args: Vec<String>, params: Vec<(String, String)>, human: bool) -> Result<QueryStatus> {
    match args.as_slice() {
        [action] if action == "list" => {
            for (name, _) in commands(workspace)? {
                println!("{name}");
            }
            Ok(QueryStatus::Ok)
        }
        [action, name] if action == "describe" => {
            let query = load(workspace, name)?;
            if human {
                println!(
                    "{}({})",
                    query.name,
                    query
                        .parameters
                        .iter()
                        .map(|p| format!("{}: {:?}", p.name, p.ty))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            } else {
                println!(
                    "{}",
                    serde_json::json!({"name": query.name, "parameters": query.parameters.iter().map(|p| format!("{}:{:?}", p.name, p.ty)).collect::<Vec<_>>() })
                );
            }
            Ok(QueryStatus::Ok)
        }
        [name] => {
            let query = load(workspace, name)?;
            let values = params.into_iter().collect::<BTreeMap<_, _>>();
            let bound = query.bind(&values)?;
            let store = IndexStore::open(IndexStore::default_path(workspace))?;
            let result = execute(&store, &query.program, &bound)?;
            print_records(&result)?;
            Ok(match result.state {
                SelectorState::Complete => QueryStatus::Ok,
                SelectorState::Partial => QueryStatus::Stale,
                SelectorState::NoResult => QueryStatus::NoResult,
            })
        }
        _ => anyhow::bail!("use `kide query list`, `kide query describe <name>`, or `kide query <name> --param name=value`"),
    }
}

fn commands(workspace: &Path) -> Result<Vec<(String, PathBuf)>> {
    let root = workspace.join(".kide/queries");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut found = fs::read_dir(root)?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|value| value.to_str()) == Some("kql")).then_some(path)
        })
        .filter_map(|path| {
            let name = path
                .file_stem()
                .and_then(|value| value.to_str())
                .map(str::to_owned)?;
            Some((name, path))
        })
        .collect::<Vec<_>>();
    found.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(found)
}

fn load(workspace: &Path, name: &str) -> Result<ProjectQuery> {
    commands(workspace)?
        .into_iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, path)| load_with_macros(workspace, &path))
        .transpose()?
        .ok_or_else(|| anyhow::anyhow!("unknown project query `{name}`"))
}

fn load_with_macros(workspace: &Path, path: &Path) -> Result<ProjectQuery> {
    let text = fs::read_to_string(path)?;
    let registry = PackageRegistry::load_workspace(workspace)?;
    let mut expanded = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(macro_name) = trimmed.strip_prefix("from ").and_then(|value| value.strip_suffix("()")) {
            let resolved = registry.resolve_macro(macro_name)?;
            let PackageSource::Workspace { manifest_path } = &resolved.package.source else { anyhow::bail!("built-in macro bodies are not installed yet"); };
            let body = manifest_path.parent().expect("manifest parent").join("queries").join(resolved.export_name.replace('.', "/")).with_extension("kql");
            expanded.push_str(&fs::read_to_string(body)?);
            expanded.push('\n');
        } else { expanded.push_str(line); expanded.push('\n'); }
    }
    ProjectQuery::parse(&expanded).map_err(Into::into)
}

fn print_records(result: &QueryResult) -> Result<()> {
    let selected = SelectorResult {
        state: result.state,
        plan: result.plan.selector_plan.clone(),
        symbols: result.symbols.clone(),
    };
    for record in records(&selected) {
        println!("{}", serde_json::to_string(&record)?);
    }
    Ok(())
}
