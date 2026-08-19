//! Command dispatcher and shared command helpers.

use crate::{Cli, Command};
use anyhow::Result;

mod index;
mod input;
mod navigation;
mod output;
mod query;
mod search;
use kide_core::QueryStatus;

pub(crate) fn dispatch(cli: Cli, human_output: bool) -> Result<QueryStatus> {
    match cli.command {
        Command::Query { args, params } => {
            query::run(&cli.workspace, args, params, human_output, cli.verbose)
        }
        Command::Index {
            path,
            force,
            warm_dependencies,
            materialize_only,
        } => index::index(
            path,
            cli.verbose,
            force,
            warm_dependencies,
            materialize_only,
        ),
        Command::Status => search::status(&cli.workspace, human_output),
        Command::Text { query } => search::text_search(&cli.workspace, query, human_output),
        Command::Symbols { query, short } => {
            search::symbols(&cli.workspace, query, short || human_output)
        }
        Command::Select {
            applies,
            kotlin_class,
            java_class,
            component,
            qualified_prefix,
        } => navigation::select_symbols(
            &cli.workspace,
            applies,
            kotlin_class,
            java_class,
            component,
            qualified_prefix,
            human_output,
        ),
        Command::Definition { target } => navigation::definition(
            &cli.workspace,
            input::target_from_argument_or_stdin(target)?,
            human_output,
        ),
        Command::Refs { target, short } => {
            navigation::fan_out(input::targets_from_argument_or_stdin(target)?, |target| {
                navigation::references(&cli.workspace, target, short || human_output)
            })
        }
        Command::Implementations { target, transitive } => {
            navigation::fan_out(input::targets_from_argument_or_stdin(target)?, |target| {
                navigation::implementations(&cli.workspace, target, transitive, human_output)
            })
        }
        Command::Callers { target } => {
            navigation::fan_out(input::targets_from_argument_or_stdin(target)?, |target| {
                navigation::callers(&cli.workspace, target, human_output)
            })
        }
        Command::TypeAt { location } => navigation::type_at(&cli.workspace, location, human_output),
    }
}

pub(crate) use output::{exit_code, print_response};

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::input::{target_from_pipe_text, target_from_symbols};
    use super::navigation::{
        TargetResolution, byte_to_location, cached_dependency_implementations, callers, fan_out,
        implementations, target_problem, type_at,
    };
    use super::{Cli, Command, exit_code};
    use std::process::ExitCode;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn implementations_can_explicitly_request_transitive_results() {
        let cli = Cli::try_parse_from(["kide", "implementations", "--transitive", "kotlin:base"])
            .expect("parses transitive implementations request");
        assert!(matches!(
            cli.command,
            Command::Implementations {
                transitive: true,
                ..
            }
        ));
    }

    #[test]
    fn persisted_navigation_queries_survive_a_cold_store_restart() {
        let workspace = tempfile::tempdir().expect("workspace");
        let source_path = "src/Main.kt";
        let text = "fun caller() = target()\n";
        std::fs::create_dir_all(workspace.path().join("src")).expect("source directory");
        std::fs::write(workspace.path().join(source_path), text).expect("source file");
        let source = kide_core::SourceUnit {
            id: kide_core::SourceUnitId::new("gradle:app:main:src/Main.kt"),
            component: kide_core::ComponentId::new("gradle:app:main"),
            path: kide_core::WorkspacePath::new(source_path),
            language: kide_core::Language::Kotlin,
            origin: kide_core::SourceOrigin::Source,
            content: kide_core::document_from_bytes(
                kide_core::WorkspacePath::new(source_path),
                text.as_bytes().to_vec(),
            )
            .expect("document")
            .fingerprint,
            context: kide_core::Fingerprint::new("sha256:context"),
        };
        let provenance = kide_core::Provenance {
            backend: "fixture".to_owned(),
            backend_version: "1".to_owned(),
            protocol_version: kide_core::WORKER_PROTOCOL_VERSION,
            analysis_options: kide_core::Fingerprint::new("sha256:options"),
        };
        let target = kide_core::SymbolId::new("kotlin:demo.Target");
        let caller = kide_core::SymbolId::new("kotlin:demo.Caller");
        let range = |start, end| kide_core::SourceRange {
            source_unit: source.id.clone(),
            bytes: kide_core::ByteRange { start, end },
        };
        let symbol = |id: kide_core::SymbolId, name: &str, start| kide_core::SymbolRecord {
            id,
            backend_key: kide_core::BackendKey {
                backend: "fixture".to_owned(),
                schema_version: 1,
                value: name.to_owned(),
            },
            language: kide_core::Language::Kotlin,
            kind: kide_core::SymbolKind::Function,
            name: name.to_owned(),
            qualified_name: Some(format!("demo.{name}")),
            signature: None,
            component: source.component.clone(),
            declaration: range(start, start + 6),
            name_range: range(start, start + 6),
            owner: None,
            modifiers: vec![],
            applied_symbols: vec![],
            freshness: kide_core::Freshness::Fresh,
            completeness: kide_core::Completeness::Complete,
            provenance: provenance.clone(),
        };
        let occurrence = kide_core::SourceOccurrence {
            range: range(15, 21),
            kind: kide_core::OccurrenceKind::Call,
            enclosing_symbol: Some(caller.clone()),
            target: Some(target.clone()),
            type_id: Some(kide_core::TypeId::new("kotlin:Unit")),
            precision: kide_core::Precision::Exact,
            freshness: kide_core::Freshness::Fresh,
            completeness: kide_core::Completeness::Complete,
            provenance: provenance.clone(),
        };
        let snapshot = kide_core::FileAnalysisSnapshot {
            source_unit: source.clone(),
            structural_fingerprint: None,
            public_api_fingerprint: None,
            symbols: vec![
                symbol(target.clone(), "target", 0),
                symbol(caller, "caller", 4),
            ],
            applications: vec![],
            occurrences: vec![occurrence.clone()],
            references: vec![],
            calls: vec![kide_core::CallEdge {
                source: occurrence.clone(),
                target: target.clone(),
                caller: occurrence.enclosing_symbol.clone(),
                precision: kide_core::Precision::Exact,
            }],
            hierarchy: vec![kide_core::HierarchyEdge {
                subtype: kide_core::SymbolId::new("kotlin:demo.Child"),
                supertype: target.clone(),
                precision: kide_core::Precision::Exact,
                provenance: provenance.clone(),
            }],
            types: vec![kide_core::TypeRecord {
                id: kide_core::TypeId::new("kotlin:Unit"),
                language: kide_core::Language::Kotlin,
                display: "Unit".to_owned(),
                backend_key: None,
                freshness: kide_core::Freshness::Fresh,
                completeness: kide_core::Completeness::Complete,
                provenance,
            }],
            diagnostics: vec![],
            completeness: kide_core::Completeness::Complete,
            provenance: kide_core::Provenance {
                backend: "fixture".to_owned(),
                backend_version: "1".to_owned(),
                protocol_version: kide_core::WORKER_PROTOCOL_VERSION,
                analysis_options: kide_core::Fingerprint::new("sha256:options"),
            },
        };
        let path = kide_core::IndexStore::default_path(workspace.path());
        let mut store = kide_core::IndexStore::open(&path).expect("store");
        store
            .replace_snapshot(&source, &snapshot)
            .expect("snapshot");
        drop(store);
        assert_eq!(
            callers(workspace.path(), target.as_str().to_owned(), false).unwrap(),
            kide_core::QueryStatus::Ok
        );
        assert_eq!(
            implementations(workspace.path(), target.as_str().to_owned(), false, false).unwrap(),
            kide_core::QueryStatus::NoResult
        );
        assert_eq!(
            type_at(workspace.path(), "src/Main.kt:1:16".to_owned(), false).unwrap(),
            kide_core::QueryStatus::Ok
        );
    }

    #[test]
    fn cached_dependency_hierarchy_falls_back_without_sqlite_snapshots() {
        let workspace = tempfile::tempdir().expect("workspace");
        let source = kide_core::SourceUnit {
            id: kide_core::SourceUnitId::new("jvm:sha256:widget"),
            component: kide_core::ComponentId::new("fixture:main"),
            path: kide_core::WorkspacePath::new(".kide/dependencies/widget"),
            language: kide_core::Language::Java,
            origin: kide_core::SourceOrigin::Dependency,
            content: kide_core::Fingerprint::new("sha256:widget"),
            context: kide_core::Fingerprint::new("sha256:context"),
        };
        let provenance = kide_core::Provenance {
            backend: "fixture".into(),
            backend_version: "1".into(),
            protocol_version: kide_core::WORKER_PROTOCOL_VERSION,
            analysis_options: kide_core::Fingerprint::new("sha256:options"),
        };
        let target = kide_core::SymbolId::new("jvm:sha256:widget:example.Widget");
        let implementation = kide_core::SymbolId::new("jvm:sha256:widget:example.WidgetImpl");
        let descriptor = kide_core::ArtifactDescriptor {
            source_unit: source.clone(),
            provenance: provenance.clone(),
            symbol_locators: vec![kide_core::SymbolLocator {
                qualified_name: "example.Widget".into(),
                symbol: target.clone(),
            }],
        };
        let store =
            kide_core::IndexStore::open(kide_core::IndexStore::default_path(workspace.path()))
                .expect("store");
        store.put_artifact_descriptor(&descriptor).expect("catalog");
        assert!(store.source_unit(&source.id).expect("store read").is_none());

        let graph = kide_core::artifact_proto::GraphArtifact {
            snapshots: vec![kide_core::artifact_proto::GraphSnapshot {
                source_unit: Some(kide_core::artifact_proto::ArtifactSourceUnit {
                    id: source.id.as_str().into(),
                    component: source.component.as_str().into(),
                    path: source.path.as_str().into(),
                    language: "java".into(),
                    origin: "dependency".into(),
                    content_fingerprint: source.content.as_str().into(),
                    context_fingerprint: source.context.as_str().into(),
                }),
                provenances: vec![kide_core::artifact_proto::ArtifactProvenance {
                    backend: provenance.backend.clone(),
                    backend_version: provenance.backend_version.clone(),
                    worker_protocol_version: provenance.protocol_version,
                    analysis_options_fingerprint: provenance.analysis_options.as_str().into(),
                }],
                provenance_index: Some(0),
                symbols: vec![
                    artifact_symbol(&target, "Widget", 0),
                    artifact_symbol(&implementation, "WidgetImpl", 7),
                ],
                hierarchy: vec![kide_core::artifact_proto::ArtifactHierarchy {
                    subtype_symbol_id: implementation.as_str().into(),
                    supertype_symbol_id: target.as_str().into(),
                    precision: "exact".into(),
                    provenance_index: Some(0),
                    subtype_symbol_ordinal: None,
                    supertype_symbol_ordinal: None,
                }],
                completeness: "partial".into(),
                ..Default::default()
            }],
        };
        let cache =
            kide_core::ArtifactBlobCache::open(workspace.path().join(".kide/artifact-cache"))
                .expect("cache");
        cache
            .publish(
                &kide_core::ArtifactBlobKey::new(source.content.clone(), &provenance),
                kide_core::artifact_blob_layout::ArtifactBlobLayout::encode(&graph).bytes(),
            )
            .expect("blob");

        assert_eq!(
            cached_dependency_implementations(&store, workspace.path(), &target)
                .expect("cached hierarchy")
                .into_iter()
                .map(|symbol| symbol.id)
                .collect::<Vec<_>>(),
            vec![implementation]
        );
        assert_eq!(
            implementations(workspace.path(), target.as_str().into(), false, false)
                .expect("CLI fallback"),
            kide_core::QueryStatus::Ok
        );
    }

    fn artifact_symbol(
        id: &kide_core::SymbolId,
        name: &str,
        start: u64,
    ) -> kide_core::artifact_proto::ArtifactSymbol {
        kide_core::artifact_proto::ArtifactSymbol {
            id: id.as_str().into(),
            backend_key: name.into(),
            backend_schema_version: 1,
            language: "java".into(),
            kind: "class".into(),
            name: name.into(),
            declaration: Some(kide_core::artifact_proto::ArtifactRange {
                start,
                end: start + name.len() as u64,
            }),
            name_range: Some(kide_core::artifact_proto::ArtifactRange {
                start,
                end: start + name.len() as u64,
            }),
            freshness: "fresh".into(),
            completeness: "partial".into(),
            component_id: "fixture:main".into(),
            provenance_index: Some(0),
            ..Default::default()
        }
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

    #[test]
    fn ambiguous_and_stale_targets_are_machine_readable_states() {
        let (status, _, problems) = target_problem(TargetResolution::Ambiguous(vec![
            kide_core::SymbolId::new("kotlin:first"),
            kide_core::SymbolId::new("kotlin:second"),
        ]));
        assert_eq!(status, kide_core::QueryStatus::Ambiguous);
        assert_eq!(problems[0].code, "ambiguous_target");
        let (status, _, problems) = target_problem(TargetResolution::Stale);
        assert_eq!(status, kide_core::QueryStatus::Stale);
        assert_eq!(problems[0].code, "stale_source_snapshot");
        assert!(problems[0].retryable);
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
