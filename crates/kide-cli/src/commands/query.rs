use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use kide_core::{
    DiscoveredWorker, Fingerprint, IndexStore, QueryCapabilityNegotiation, QueryCapabilityStatus,
    QueryStatus, SemanticQueryArgument, SemanticQueryArgumentValue, SemanticQueryBudget,
    SemanticQueryResponse, SemanticQueryResponseState, SemanticQueryResultKind, WorkerRegistry,
    WorkerSupervisor, execute_semantic_capability, negotiate_query_capabilities,
    plan_semantic_capability,
    project_query::ProjectQuery,
    query_package::{PackageCapability, PackageRegistry, PackageSource, RegisteredPackage},
    selector::{SelectorResult, SelectorState, records},
    semantic_query::{QueryCapabilityValue, QueryParameters, QueryResult, QueryValue, execute},
};
use serde::Serialize;

use super::index::kotlin_worker_installation;

const QUERY_CAPABILITY_MAX_BYTES: u64 = 1024 * 1024;
const QUERY_CAPABILITY_DEADLINE_MILLIS: u64 = 5_000;

#[derive(Debug, Clone)]
struct ResolvedProjectQuery {
    query: ProjectQuery,
    packages: Vec<ResolvedPackage>,
}

#[derive(Debug, Clone)]
struct ResolvedPackage {
    id: String,
    version: String,
    manifest_digest: Fingerprint,
    capabilities: Vec<PackageCapability>,
}

#[derive(Debug, Clone, Serialize)]
struct QueryPlanRecord {
    packages: Vec<PackagePlanRecord>,
    capabilities: Vec<CapabilityPlanRecord>,
}

#[derive(Debug, Clone, Serialize)]
struct PackagePlanRecord {
    id: String,
    version: String,
    manifest_digest: Fingerprint,
}

#[derive(Debug, Clone, Serialize)]
struct CapabilityPlanRecord {
    name: String,
    version: u32,
    required: bool,
    used: bool,
    status: CapabilityPlanStatus,
    providers: Vec<CapabilityProviderRecord>,
    available_versions: Vec<u32>,
    budget: Option<SemanticQueryBudget>,
    execution_state: Option<SemanticQueryResponseState>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum CapabilityPlanStatus {
    Pending,
    Supported,
    Missing,
    IncompatibleVersion,
}

#[derive(Debug, Clone, Serialize)]
struct CapabilityProviderRecord {
    installation: String,
    backend: String,
    backend_version: String,
    protocol_version: u32,
}

pub(super) fn run(
    workspace: &Path,
    args: Vec<String>,
    params: Vec<(String, String)>,
    human: bool,
    verbosity: u8,
) -> Result<QueryStatus> {
    match args.as_slice() {
        [action] if action == "list" => {
            for (name, _) in commands(workspace)? {
                println!("{name}");
            }
            Ok(QueryStatus::Ok)
        }
        [action, name] if action == "describe" => {
            let resolved = load(workspace, name)?;
            let plan = static_plan(&resolved);
            if human {
                println!(
                    "{}({})",
                    resolved.query.name,
                    resolved
                        .query
                        .parameters
                        .iter()
                        .map(|p| format!("{}: {:?}", p.name, p.ty))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                for package in &plan.packages {
                    println!(
                        "package {}@{} ({})",
                        package.id,
                        package.version,
                        package.manifest_digest.as_str()
                    );
                }
            } else {
                println!(
                    "{}",
                    serde_json::json!({
                        "name": resolved.query.name,
                        "parameters": resolved.query.parameters.iter().map(|p| format!("{}:{:?}", p.name, p.ty)).collect::<Vec<_>>(),
                        "plan": plan,
                    })
                );
            }
            Ok(QueryStatus::Ok)
        }
        [name] => invoke(workspace, name, params, verbosity),
        _ => bail!(
            "use `kide query list`, `kide query describe <name>`, or `kide query <name> --param name=value`"
        ),
    }
}

fn invoke(
    workspace: &Path,
    name: &str,
    params: Vec<(String, String)>,
    verbosity: u8,
) -> Result<QueryStatus> {
    let resolved = load(workspace, name)?;
    let values = params.into_iter().collect::<BTreeMap<_, _>>();
    let bound = resolved.query.bind(&values)?;
    let requirements = requirements(&resolved);
    let used = used_capabilities(&resolved);

    // Plain indexed Java/Kotlin paths stay worker-free. Package requirements
    // trigger only a static handshake; execution is reserved for explicit
    // `using capability ...` steps.
    let needs_discovery = requirements
        .iter()
        .any(|requirement| requirement.required || used.contains(&requirement.name));
    let workers = if !needs_discovery {
        Vec::new()
    } else {
        WorkerRegistry::new(vec![kotlin_worker_installation(workspace, verbosity)?]).discover()?
    };
    let negotiations = negotiate_query_capabilities(&requirements, &workers);
    execute_resolved(
        workspace,
        resolved,
        bound,
        workers,
        negotiations,
        |worker, plan| {
            let mut supervisor = WorkerSupervisor::new(worker.installation.launch.clone());
            execute_semantic_capability(
                &mut supervisor,
                plan,
                format!(
                    "project-query-{}-{}",
                    plan.request.capability_name, plan.request.capability_version
                ),
            )
            .map_err(Into::into)
        },
    )
}

fn execute_resolved(
    workspace: &Path,
    resolved: ResolvedProjectQuery,
    bound: QueryParameters,
    workers: Vec<DiscoveredWorker>,
    negotiations: Vec<QueryCapabilityNegotiation>,
    mut execute_capability: impl FnMut(
        &DiscoveredWorker,
        &kide_core::SemanticCapabilityPlan,
    ) -> Result<SemanticQueryResponse>,
) -> Result<QueryStatus> {
    let requirements = requirements(&resolved);
    let used = used_capabilities(&resolved);
    let mut plan = negotiated_plan(&resolved, &negotiations, &workers);
    if negotiations
        .iter()
        .any(|item| item.required && item.status != QueryCapabilityStatus::Supported)
    {
        print_status_record(QueryStatus::Unsupported, &plan)?;
        return Ok(QueryStatus::Unsupported);
    }

    let store = IndexStore::open(IndexStore::default_path(workspace))?;
    let mut result = execute(&store, &resolved.query.program, &bound)?;
    let mut capability_provenance = Vec::new();
    let mut capability_partial = negotiations.iter().any(|item| {
        !item.required
            && used.contains(&item.name)
            && item.status != QueryCapabilityStatus::Supported
    });

    for step in &resolved.query.program.capability_steps {
        let requirement = requirement_for_step(&requirements, &step.name)?;
        let negotiation = negotiations
            .iter()
            .find(|item| item.name == step.name && item.version == requirement.version)
            .expect("every requirement is negotiated");
        if negotiation.status != QueryCapabilityStatus::Supported {
            continue;
        }
        let provider_name = negotiation
            .providers
            .first()
            .expect("supported negotiation has a provider");
        let worker = workers
            .iter()
            .find(|worker| &worker.installation.name == provider_name)
            .expect("negotiation provider is discovered");
        let budget = capability_budget(&result);
        let capability_plan = plan_semantic_capability(
            worker,
            &step.name,
            requirement.version,
            bind_capability_arguments(&store, &bound, step)?,
            result
                .symbols
                .iter()
                .map(|symbol| symbol.id.clone())
                .collect(),
            result
                .symbols
                .iter()
                .map(|symbol| symbol.declaration.source_unit.clone())
                .collect(),
            budget,
        )?;
        if capability_plan.capability.result != SemanticQueryResultKind::CandidateSymbols {
            bail!(
                "capability `{}` returns normalized facts and cannot be used as a runtime candidate refinement",
                step.name
            );
        }
        let response = execute_capability(worker, &capability_plan)?;
        if let Some(record) = plan
            .capabilities
            .iter_mut()
            .find(|record| record.name == step.name && record.version == requirement.version)
        {
            record.budget = Some(budget);
            record.execution_state = Some(response.state);
        }
        match response.state {
            SemanticQueryResponseState::Unsupported if requirement.required => {
                print_status_record(QueryStatus::Unsupported, &plan)?;
                return Ok(QueryStatus::Unsupported);
            }
            SemanticQueryResponseState::Unsupported | SemanticQueryResponseState::Partial => {
                capability_partial = true;
            }
            SemanticQueryResponseState::Complete => {}
        }
        if response.state != SemanticQueryResponseState::Unsupported {
            let selected = response
                .candidate_symbols
                .iter()
                .map(|symbol| symbol.as_str())
                .collect::<BTreeSet<_>>();
            result
                .symbols
                .retain(|symbol| selected.contains(symbol.id.as_str()));
        }
        capability_provenance.push(response.provenance);
    }

    if capability_partial && result.state == SelectorState::Complete {
        result.state = SelectorState::Partial;
    }
    let status = match result.state {
        SelectorState::Complete => QueryStatus::Ok,
        SelectorState::Partial => QueryStatus::Stale,
        SelectorState::NoResult => QueryStatus::NoResult,
    };
    print_records(&result, status, &plan, &capability_provenance)?;
    Ok(status)
}

fn requirement_for_step<'a>(
    requirements: &'a [PackageCapability],
    name: &str,
) -> Result<&'a PackageCapability> {
    let matches = requirements
        .iter()
        .filter(|requirement| requirement.name == name)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => bail!("capability `{name}` is not declared by the selected package path"),
        [requirement] => Ok(*requirement),
        _ => bail!("capability `{name}` has conflicting versions on the selected package path"),
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

fn load(workspace: &Path, name: &str) -> Result<ResolvedProjectQuery> {
    commands(workspace)?
        .into_iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, path)| load_with_macros(workspace, &path))
        .transpose()?
        .ok_or_else(|| anyhow::anyhow!("unknown project query `{name}`"))
}

fn load_with_macros(workspace: &Path, path: &Path) -> Result<ResolvedProjectQuery> {
    let text = fs::read_to_string(path)?;
    let registry = PackageRegistry::load_workspace(workspace)?;
    let mut expanded = String::new();
    let mut packages = BTreeMap::<String, ResolvedPackage>::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(macro_name) = trimmed
            .strip_prefix("from ")
            .and_then(|value| value.strip_suffix("()"))
        {
            let resolved = registry.resolve_macro(macro_name)?;
            let PackageSource::Workspace { manifest_path } = &resolved.package.source else {
                bail!("built-in macro bodies are not installed yet");
            };
            let body = manifest_path
                .parent()
                .expect("manifest parent")
                .join("queries")
                .join(resolved.export_name.replace('.', "/"))
                .with_extension("kql");
            expanded.push_str(&fs::read_to_string(body)?);
            expanded.push('\n');
            packages.insert(
                resolved.package.manifest.id.clone(),
                package_resolution(resolved.package),
            );
        } else {
            expanded.push_str(line);
            expanded.push('\n');
        }
    }
    let query = ProjectQuery::parse(&expanded)?;
    for step in &query.program.capability_steps {
        if !packages.values().any(|package| {
            package
                .capabilities
                .iter()
                .any(|capability| capability.name == step.name)
        }) {
            bail!(
                "capability `{}` is not declared by a package on the selected query path",
                step.name
            );
        }
    }
    Ok(ResolvedProjectQuery {
        query,
        packages: packages.into_values().collect(),
    })
}

fn package_resolution(package: &RegisteredPackage) -> ResolvedPackage {
    ResolvedPackage {
        id: package.manifest.id.clone(),
        version: package.manifest.version.clone(),
        manifest_digest: package.manifest_digest.clone(),
        capabilities: package.manifest.capabilities.clone(),
    }
}

fn requirements(resolved: &ResolvedProjectQuery) -> Vec<PackageCapability> {
    let mut merged = BTreeMap::<(String, u32), bool>::new();
    for capability in resolved
        .packages
        .iter()
        .flat_map(|package| &package.capabilities)
    {
        merged
            .entry((capability.name.clone(), capability.version))
            .and_modify(|required| *required |= capability.required)
            .or_insert(capability.required);
    }
    merged
        .into_iter()
        .map(|((name, version), required)| PackageCapability {
            name,
            version,
            required,
        })
        .collect()
}

fn used_capabilities(resolved: &ResolvedProjectQuery) -> BTreeSet<String> {
    resolved
        .query
        .program
        .capability_steps
        .iter()
        .map(|step| step.name.clone())
        .collect()
}

fn static_plan(resolved: &ResolvedProjectQuery) -> QueryPlanRecord {
    let used = used_capabilities(resolved);
    QueryPlanRecord {
        packages: package_records(resolved),
        capabilities: requirements(resolved)
            .into_iter()
            .map(|requirement| CapabilityPlanRecord {
                used: used.contains(&requirement.name),
                name: requirement.name,
                version: requirement.version,
                required: requirement.required,
                status: CapabilityPlanStatus::Pending,
                providers: Vec::new(),
                available_versions: Vec::new(),
                budget: None,
                execution_state: None,
            })
            .collect(),
    }
}

fn negotiated_plan(
    resolved: &ResolvedProjectQuery,
    negotiations: &[QueryCapabilityNegotiation],
    workers: &[DiscoveredWorker],
) -> QueryPlanRecord {
    let used = used_capabilities(resolved);
    QueryPlanRecord {
        packages: package_records(resolved),
        capabilities: negotiations
            .iter()
            .map(|negotiation| CapabilityPlanRecord {
                name: negotiation.name.clone(),
                version: negotiation.version,
                required: negotiation.required,
                used: used.contains(&negotiation.name),
                status: match negotiation.status {
                    QueryCapabilityStatus::Supported => CapabilityPlanStatus::Supported,
                    QueryCapabilityStatus::Missing => CapabilityPlanStatus::Missing,
                    QueryCapabilityStatus::IncompatibleVersion => {
                        CapabilityPlanStatus::IncompatibleVersion
                    }
                },
                providers: negotiation
                    .providers
                    .iter()
                    .filter_map(|name| {
                        workers
                            .iter()
                            .find(|worker| &worker.installation.name == name)
                    })
                    .map(|worker| CapabilityProviderRecord {
                        installation: worker.installation.name.clone(),
                        backend: worker.capabilities.identity.backend.clone(),
                        backend_version: worker.capabilities.identity.backend_version.clone(),
                        protocol_version: worker.capabilities.protocol_version,
                    })
                    .collect(),
                available_versions: negotiation.available_versions.clone(),
                budget: None,
                execution_state: None,
            })
            .collect(),
    }
}

fn package_records(resolved: &ResolvedProjectQuery) -> Vec<PackagePlanRecord> {
    resolved
        .packages
        .iter()
        .map(|package| PackagePlanRecord {
            id: package.id.clone(),
            version: package.version.clone(),
            manifest_digest: package.manifest_digest.clone(),
        })
        .collect()
}

fn capability_budget(result: &QueryResult) -> SemanticQueryBudget {
    let candidates = u32::try_from(result.symbols.len())
        .unwrap_or(u32::MAX)
        .max(1);
    let source_units = result
        .symbols
        .iter()
        .map(|symbol| symbol.declaration.source_unit.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    SemanticQueryBudget {
        max_candidates: candidates,
        max_nodes: u32::try_from(source_units).unwrap_or(u32::MAX).max(1),
        max_bytes: QUERY_CAPABILITY_MAX_BYTES,
        deadline_millis: QUERY_CAPABILITY_DEADLINE_MILLIS,
    }
}

fn bind_capability_arguments(
    store: &IndexStore,
    parameters: &QueryParameters,
    step: &kide_core::semantic_query::QueryCapabilityStep,
) -> Result<Vec<SemanticQueryArgument>> {
    step.arguments
        .iter()
        .map(|argument| {
            let value = match &argument.value {
                QueryCapabilityValue::Parameter(name) => match parameters
                    .get(name)
                    .with_context(|| format!("query parameter `{name}` is required"))?
                {
                    QueryValue::SymbolId(value) => {
                        SemanticQueryArgumentValue::SymbolId(value.clone())
                    }
                    QueryValue::QualifiedSymbol(value) => SemanticQueryArgumentValue::SymbolId(
                        resolve_qualified_symbol(store, value)?,
                    ),
                    QueryValue::ComponentId(value) => {
                        SemanticQueryArgumentValue::ComponentId(value.clone())
                    }
                    QueryValue::String(value) => SemanticQueryArgumentValue::String(value.clone()),
                    QueryValue::Integer(value) => {
                        SemanticQueryArgumentValue::Integer(i64::from(*value))
                    }
                },
                QueryCapabilityValue::SymbolId(value) => {
                    SemanticQueryArgumentValue::SymbolId(value.clone())
                }
                QueryCapabilityValue::ComponentId(value) => {
                    SemanticQueryArgumentValue::ComponentId(value.clone())
                }
                QueryCapabilityValue::String(value) => {
                    SemanticQueryArgumentValue::String(value.clone())
                }
                QueryCapabilityValue::Integer(value) => SemanticQueryArgumentValue::Integer(*value),
            };
            Ok(SemanticQueryArgument {
                name: argument.name.clone(),
                value,
            })
        })
        .collect()
}

fn resolve_qualified_symbol(store: &IndexStore, name: &str) -> Result<kide_core::SymbolId> {
    match store.symbols_with_qualified_name(name)?.as_slice() {
        [] if name.contains('.') => Ok(kide_core::SymbolId::new(format!("jvm:type:{name}"))),
        [] => bail!("qualified symbol `{name}` did not resolve"),
        [symbol] => Ok(symbol.clone()),
        symbols => bail!(
            "qualified symbol `{name}` is ambiguous: {} candidates",
            symbols.len()
        ),
    }
}

fn print_records(
    result: &QueryResult,
    status: QueryStatus,
    plan: &QueryPlanRecord,
    capability_provenance: &[kide_core::Provenance],
) -> Result<()> {
    let selected = SelectorResult {
        state: result.state,
        plan: result.plan.selector_plan.clone(),
        symbols: result.symbols.clone(),
    };
    let records = records(&selected);
    if records.is_empty() {
        return print_status_record(status, plan);
    }
    for mut record in records {
        record
            .metadata
            .provenance
            .extend_from_slice(capability_provenance);
        let mut value = serde_json::to_value(record)?;
        let object = value.as_object_mut().expect("selector record is an object");
        object.insert("status".to_owned(), serde_json::to_value(status)?);
        object.insert("state".to_owned(), serde_json::json!(result_state(status)));
        object.insert("plan".to_owned(), serde_json::to_value(plan)?);
        println!("{}", serde_json::to_string(&value)?);
    }
    Ok(())
}

fn print_status_record(status: QueryStatus, plan: &QueryPlanRecord) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "status": status,
            "state": result_state(status),
            "symbol": null,
            "plan": plan,
        }))?
    );
    Ok(())
}

fn result_state(status: QueryStatus) -> &'static str {
    match status {
        QueryStatus::Ok => "complete",
        QueryStatus::Stale => "partial",
        QueryStatus::Unsupported => "unsupported",
        QueryStatus::NoResult => "no_result",
        QueryStatus::Ambiguous => "ambiguous",
        QueryStatus::InvalidRequest => "invalid_request",
        QueryStatus::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kide_core::{
        BuildSystem, Language, Provenance, SemanticQueryCapability, SemanticQueryResultKind,
        WORKER_PROTOCOL_VERSION, WorkerCapabilities, WorkerIdentity, WorkerInstallation,
        WorkerLaunch,
    };

    #[test]
    fn describe_plan_is_static_and_keeps_package_provenance() {
        let workspace = tempfile::tempdir().expect("workspace");
        write_package(
            workspace.path(),
            "required = true",
            "using capability fixture.echo()",
        );
        let resolved = load(workspace.path(), "demo.command").expect("loads project query");
        let plan = static_plan(&resolved);
        assert_eq!(plan.packages[0].id, "fixture.package");
        assert!(matches!(
            plan.capabilities[0].status,
            CapabilityPlanStatus::Pending
        ));
        assert!(plan.capabilities[0].used);
    }

    #[test]
    fn missing_optional_capability_only_matters_when_selected() {
        let resolved = ResolvedProjectQuery {
            query: ProjectQuery::parse(
                "command demo.command() {\nfrom applies(symbol-id(\"annotation\"))\nreturn symbol\nlimit 10\n}",
            )
            .expect("query"),
            packages: vec![ResolvedPackage {
                id: "fixture.package".to_owned(),
                version: "1.0.0".to_owned(),
                manifest_digest: Fingerprint::new("sha256:fixture"),
                capabilities: vec![PackageCapability {
                    name: "fixture.optional".to_owned(),
                    version: 1,
                    required: false,
                }],
            }],
        };
        let negotiations = negotiate_query_capabilities(&requirements(&resolved), &[]);
        assert_eq!(negotiations[0].status, QueryCapabilityStatus::Missing);
        assert!(used_capabilities(&resolved).is_empty());
    }

    #[test]
    fn missing_required_capability_is_a_stable_unsupported_result() {
        let workspace = tempfile::tempdir().expect("workspace");
        write_package(workspace.path(), "required = true", "");
        let resolved = load(workspace.path(), "demo.command").expect("loads query");
        let negotiations = negotiate_query_capabilities(&requirements(&resolved), &[]);
        let status = execute_resolved(
            workspace.path(),
            resolved,
            QueryParameters::new(),
            Vec::new(),
            negotiations,
            |_, _| panic!("missing capability must not execute"),
        )
        .expect("returns structured status");
        assert_eq!(status, QueryStatus::Unsupported);
    }

    #[test]
    fn missing_selected_optional_capability_skips_execution_without_failing() {
        let workspace = tempfile::tempdir().expect("workspace");
        write_package(
            workspace.path(),
            "required = false",
            "using capability fixture.echo()",
        );
        let resolved = load(workspace.path(), "demo.command").expect("loads query");
        let negotiations = negotiate_query_capabilities(&requirements(&resolved), &[]);
        let status = execute_resolved(
            workspace.path(),
            resolved,
            QueryParameters::new(),
            Vec::new(),
            negotiations,
            |_, _| panic!("missing optional capability must not execute"),
        )
        .expect("continues with the indexed path");
        assert_eq!(status, QueryStatus::NoResult);
    }

    #[test]
    fn selected_capability_step_reaches_the_bounded_executor() {
        let workspace = tempfile::tempdir().expect("workspace");
        write_package(
            workspace.path(),
            "required = true",
            "using capability fixture.echo()",
        );
        let resolved = load(workspace.path(), "demo.command").expect("loads query");
        let worker = fixture_worker();
        let negotiations =
            negotiate_query_capabilities(&requirements(&resolved), std::slice::from_ref(&worker));
        let mut calls = 0;
        let status = execute_resolved(
            workspace.path(),
            resolved,
            QueryParameters::new(),
            vec![worker],
            negotiations,
            |worker, plan| {
                calls += 1;
                assert_eq!(plan.request.capability_name, "fixture.echo");
                assert_eq!(plan.request.budget.max_candidates, 1);
                Ok(SemanticQueryResponse {
                    capability_name: "fixture.echo".to_owned(),
                    capability_version: 1,
                    state: SemanticQueryResponseState::Complete,
                    candidate_symbols: Vec::new(),
                    snapshots: Vec::new(),
                    provenance: Provenance {
                        backend: worker.capabilities.identity.backend.clone(),
                        backend_version: worker.capabilities.identity.backend_version.clone(),
                        protocol_version: WORKER_PROTOCOL_VERSION,
                        analysis_options: Fingerprint::new("sha256:fixture"),
                    },
                    visited_nodes: 0,
                    produced_bytes: 0,
                })
            },
        )
        .expect("executes query plan");
        assert_eq!(calls, 1);
        assert_eq!(status, QueryStatus::NoResult);
    }

    fn fixture_worker() -> DiscoveredWorker {
        DiscoveredWorker {
            installation: WorkerInstallation {
                name: "fixture".to_owned(),
                launch: WorkerLaunch::new(std::env::current_exe().expect("test executable")),
                build_systems: vec![BuildSystem::Filesystem],
            },
            capabilities: WorkerCapabilities {
                identity: WorkerIdentity {
                    backend: "fixture".to_owned(),
                    backend_version: "1.0.0".to_owned(),
                },
                protocol_version: WORKER_PROTOCOL_VERSION,
                languages: vec![Language::Kotlin, Language::Java],
                capabilities: Vec::new(),
                semantic_query_capabilities: vec![SemanticQueryCapability {
                    name: "fixture.echo".to_owned(),
                    version: 1,
                    parameters: Vec::new(),
                    result: SemanticQueryResultKind::CandidateSymbols,
                }],
            },
        }
    }

    fn write_package(workspace: &Path, requirement: &str, capability_step: &str) {
        let package = workspace.join(".kide/query-packages/fixture.package");
        fs::create_dir_all(package.join("queries")).expect("package directories");
        fs::create_dir_all(workspace.join(".kide/queries")).expect("query directory");
        fs::write(
            package.join("package.toml"),
            format!(
                "format = 1\nid = \"fixture.package\"\nversion = \"1.0.0\"\nrequires_core = \"^1\"\n\n[exports]\nmacros = [\"echo\"]\ncommands = []\n\n[[capabilities]]\nname = \"fixture.echo\"\n{requirement}\n"
            ),
        )
        .expect("manifest");
        fs::write(
            package.join("queries/echo.kql"),
            format!("from applies(symbol-id(\"annotation\"))\n{capability_step}\n"),
        )
        .expect("macro");
        fs::write(
            workspace.join(".kide/queries/demo.command.kql"),
            "command demo.command() {\nfrom fixture.package.echo()\nreturn symbol\nlimit 10\n}\n",
        )
        .expect("project query");
    }
}
