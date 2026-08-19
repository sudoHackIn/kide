//! Discovery and selection of disposable workers.
//!
//! The registry is deliberately a Core concern: it maps a locally installed
//! executable to the build contexts it can serve, then trusts the worker's
//! handshake for its language and protocol capabilities.  Launch arguments
//! remain process-local and never enter the canonical worker protocol.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::{
    query_package::PackageCapability,
    BuildSystem, ComponentId, Language, ProjectManifest, SourceUnit, WorkerCapabilities,
    WorkerCapability, WorkerLaunch, WorkerSupervisor, WorkerSupervisorError,
};

/// Default for callers that have not yet supplied workspace scheduling policy.
pub const DEFAULT_MAX_SOURCE_UNITS_PER_WORKER_BATCH: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryCapabilityStatus {
    Supported,
    Missing,
    IncompatibleVersion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryCapabilitySupport {
    Complete,
    Partial,
    Unsupported,
}

/// Deterministic negotiation record retained by a later resolved query plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryCapabilityNegotiation {
    pub name: String,
    pub version: u32,
    pub required: bool,
    pub status: QueryCapabilityStatus,
    pub providers: Vec<String>,
    pub available_versions: Vec<u32>,
}

/// A locally discoverable worker installation.  Build-system compatibility is
/// installation metadata because it describes how Core may launch the worker;
/// language and feature compatibility always come from its handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerInstallation {
    pub name: String,
    pub launch: WorkerLaunch,
    pub build_systems: Vec<BuildSystem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredWorker {
    pub installation: WorkerInstallation,
    pub capabilities: WorkerCapabilities,
}

/// A cold batch has exactly one worker and one component build context.  Core
/// may schedule these independently without ever routing an unsupported file
/// to a superficially similar backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerBatch {
    pub worker: DiscoveredWorker,
    pub component: ComponentId,
    pub language: Language,
    pub source_units: Vec<SourceUnit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedBatch {
    pub component: ComponentId,
    pub language: Language,
    pub build_system: BuildSystem,
    pub source_units: Vec<SourceUnit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkerSelection {
    pub batches: Vec<WorkerBatch>,
    pub unsupported: Vec<UnsupportedBatch>,
}

#[derive(Debug, Clone, Default)]
pub struct WorkerRegistry {
    installations: Vec<WorkerInstallation>,
}

#[derive(Debug, Error)]
pub enum WorkerRegistryError {
    #[error("worker {worker} handshake failed: {source}")]
    Handshake {
        worker: String,
        #[source]
        source: WorkerSupervisorError,
    },
}

impl WorkerRegistry {
    pub fn new(installations: Vec<WorkerInstallation>) -> Self {
        Self { installations }
    }

    /// Starts each candidate only long enough to obtain static capabilities.
    /// Dropping its supervisor immediately keeps discovery cold and prevents a
    /// registry probe from becoming a hidden resident language service.
    pub fn discover(&self) -> Result<Vec<DiscoveredWorker>, WorkerRegistryError> {
        self.installations
            .iter()
            .cloned()
            .map(|installation| {
                let mut supervisor = WorkerSupervisor::new(installation.launch.clone());
                let capabilities =
                    supervisor
                        .handshake("worker-registry-handshake")
                        .map_err(|source| WorkerRegistryError::Handshake {
                            worker: installation.name.clone(),
                            source,
                        })?;
                Ok(DiscoveredWorker {
                    installation,
                    capabilities: capabilities.capabilities,
                })
            })
            .collect()
    }

    /// Selects one compatible worker for every `(component, language)` input
    /// group.  The stable installation-name tie-breaker makes scheduling
    /// deterministic when two workers advertise the same support.
    pub fn select(
        &self,
        manifest: &ProjectManifest,
        sources: Vec<SourceUnit>,
        required: &[WorkerCapability],
    ) -> Result<WorkerSelection, WorkerRegistryError> {
        self.select_with_batch_limit(
            manifest,
            sources,
            required,
            DEFAULT_MAX_SOURCE_UNITS_PER_WORKER_BATCH,
        )
    }

    /// The batch limit is caller-owned scheduling policy. It deliberately is
    /// not part of the worker protocol, so a future workspace config can tune
    /// it without changing worker compatibility.
    pub fn select_with_batch_limit(
        &self,
        manifest: &ProjectManifest,
        sources: Vec<SourceUnit>,
        required: &[WorkerCapability],
        max_source_units_per_batch: usize,
    ) -> Result<WorkerSelection, WorkerRegistryError> {
        assert!(max_source_units_per_batch > 0, "worker batch limit must be positive");
        let discovered = self.discover()?;
        Ok(select_discovered_with_batch_limit(
            manifest,
            sources,
            required,
            discovered,
            max_source_units_per_batch,
        ))
    }
}

pub fn select_discovered(
    manifest: &ProjectManifest,
    sources: Vec<SourceUnit>,
    required: &[WorkerCapability],
    workers: Vec<DiscoveredWorker>,
) -> WorkerSelection {
    select_discovered_with_batch_limit(
        manifest,
        sources,
        required,
        workers,
        DEFAULT_MAX_SOURCE_UNITS_PER_WORKER_BATCH,
    )
}

pub fn select_discovered_with_batch_limit(
    manifest: &ProjectManifest,
    mut sources: Vec<SourceUnit>,
    required: &[WorkerCapability],
    mut workers: Vec<DiscoveredWorker>,
    max_source_units_per_batch: usize,
) -> WorkerSelection {
    assert!(max_source_units_per_batch > 0, "worker batch limit must be positive");
    sources.sort_by_key(|source| {
        (
            source.component.as_str().to_owned(),
            source.path.as_str().to_owned(),
        )
    });
    workers.sort_by_key(|worker| worker.installation.name.clone());
    let components = manifest
        .components
        .iter()
        .map(|component| {
            (
                component.id.as_str().to_owned(),
                component.build_system.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut grouped = BTreeMap::<(String, String), Vec<SourceUnit>>::new();
    for source in sources {
        grouped
            .entry((
                source.component.as_str().to_owned(),
                language_key(&source.language),
            ))
            .or_default()
            .push(source);
    }

    let mut selection = WorkerSelection::default();
    for ((_component_key, _language_key), source_units) in grouped {
        let component = source_units[0].component.clone();
        let language = source_units[0].language.clone();
        let build_system = components
            .get(component.as_str())
            .cloned()
            // Discovery's fallback components should always be present, but a
            // malformed external manifest is still explicit rather than Kotlin.
            .unwrap_or(BuildSystem::Filesystem);
        let worker = workers.iter().find(|worker| {
            worker.installation.build_systems.contains(&build_system)
                && worker.capabilities.languages.contains(&language)
                && required
                    .iter()
                    .all(|capability| worker.capabilities.capabilities.contains(capability))
        });
        match worker {
            Some(worker) => source_units.chunks(max_source_units_per_batch).for_each(|chunk| {
                selection.batches.push(WorkerBatch {
                    worker: worker.clone(),
                    component: component.clone(),
                    language: language.clone(),
                    source_units: chunk.to_vec(),
                });
            }),
            None => selection.unsupported.push(UnsupportedBatch {
                component,
                language,
                build_system,
                source_units,
            }),
        }
    }
    selection
}

/// Matches declarative package requirements against static worker handshakes.
/// This performs no worker query and starts no compiler session.
pub fn negotiate_query_capabilities(
    requirements: &[PackageCapability],
    workers: &[DiscoveredWorker],
) -> Vec<QueryCapabilityNegotiation> {
    let mut requirements = requirements.to_vec();
    requirements.sort_by(|left, right| {
        (&left.name, left.version, left.required).cmp(&(
            &right.name,
            right.version,
            right.required,
        ))
    });
    requirements
        .into_iter()
        .map(|requirement| {
            let mut providers = BTreeSet::new();
            let mut available_versions = BTreeSet::new();
            for worker in workers {
                for capability in &worker.capabilities.semantic_query_capabilities {
                    if capability.name == requirement.name {
                        available_versions.insert(capability.version);
                        if capability.version == requirement.version {
                            providers.insert(worker.installation.name.clone());
                        }
                    }
                }
            }
            let status = if !providers.is_empty() {
                QueryCapabilityStatus::Supported
            } else if available_versions.is_empty() {
                QueryCapabilityStatus::Missing
            } else {
                QueryCapabilityStatus::IncompatibleVersion
            };
            QueryCapabilityNegotiation {
                name: requirement.name,
                version: requirement.version,
                required: requirement.required,
                status,
                providers: providers.into_iter().collect(),
                available_versions: available_versions.into_iter().collect(),
            }
        })
        .collect()
}

pub fn query_capability_support(
    negotiations: &[QueryCapabilityNegotiation],
) -> QueryCapabilitySupport {
    if negotiations
        .iter()
        .any(|item| item.required && item.status != QueryCapabilityStatus::Supported)
    {
        QueryCapabilitySupport::Unsupported
    } else if negotiations
        .iter()
        .any(|item| item.status != QueryCapabilityStatus::Supported)
    {
        QueryCapabilitySupport::Partial
    } else {
        QueryCapabilitySupport::Complete
    }
}

fn language_key(language: &Language) -> String {
    match language {
        Language::Kotlin => "kotlin".to_owned(),
        Language::Java => "java".to_owned(),
        Language::Rust => "rust".to_owned(),
        Language::TypeScript => "typescript".to_owned(),
        Language::Other(value) => format!("other:{value}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Component, Fingerprint, ProjectManifest, Provenance, SourceOrigin, SourceUnitId,
        SemanticQueryCapability, SemanticQueryParameter, SemanticQueryParameterType,
        SemanticQueryResultKind, WorkerIdentity, WorkspaceId, WorkspacePath,
        WORKER_PROTOCOL_VERSION,
    };

    fn source(component: &str, language: Language, path: &str) -> SourceUnit {
        SourceUnit {
            id: SourceUnitId::new(path),
            component: ComponentId::new(component),
            path: WorkspacePath::new(path),
            language,
            origin: SourceOrigin::Source,
            content: Fingerprint::new("sha256:source"),
            context: Fingerprint::new("sha256:context"),
        }
    }
    fn worker(
        name: &str,
        languages: Vec<Language>,
        build_systems: Vec<BuildSystem>,
    ) -> DiscoveredWorker {
        DiscoveredWorker {
            installation: WorkerInstallation {
                name: name.into(),
                launch: WorkerLaunch::new("fixture"),
                build_systems,
            },
            capabilities: WorkerCapabilities {
                identity: WorkerIdentity {
                    backend: name.into(),
                    backend_version: "test".into(),
                },
                protocol_version: WORKER_PROTOCOL_VERSION,
                languages,
                capabilities: vec![
                    WorkerCapability::Handshake,
                    WorkerCapability::FileAnalysisSnapshot,
                ],
                semantic_query_capabilities: Vec::new(),
            },
        }
    }
    fn manifest() -> ProjectManifest {
        ProjectManifest {
            workspace: WorkspaceId::new("fixture"),
            root: WorkspacePath::new("."),
            components: vec![
                Component {
                    id: ComponentId::new("gradle:kotlin"),
                    name: "kotlin".into(),
                    build_system: BuildSystem::Gradle,
                    root: WorkspacePath::new("."),
                    languages: vec![Language::Kotlin],
                    configuration: Fingerprint::new("sha256:config"),
                    source_sets: vec![],
                    classpath: vec![],
                    toolchain: None,
                    compiler_configuration: None,
                },
                Component {
                    id: ComponentId::new("cargo:rust"),
                    name: "rust".into(),
                    build_system: BuildSystem::Cargo,
                    root: WorkspacePath::new("crate"),
                    languages: vec![Language::Rust],
                    configuration: Fingerprint::new("sha256:config"),
                    source_sets: vec![],
                    classpath: vec![],
                    toolchain: None,
                    compiler_configuration: None,
                },
            ],
            dependencies: vec![],
            fingerprint: Fingerprint::new("sha256:config"),
            provenance: Provenance {
                backend: "fixture".into(),
                backend_version: "test".into(),
                protocol_version: WORKER_PROTOCOL_VERSION,
                analysis_options: Fingerprint::new("sha256:config"),
            },
        }
    }
    #[test]
    fn batches_mixed_workspace_by_language_and_build_context() {
        let selection = select_discovered(
            &manifest(),
            vec![
                source("gradle:kotlin", Language::Kotlin, "src/App.kt"),
                source("cargo:rust", Language::Rust, "crate/lib.rs"),
            ],
            &[WorkerCapability::FileAnalysisSnapshot],
            vec![
                worker("rust", vec![Language::Rust], vec![BuildSystem::Cargo]),
                worker("kotlin", vec![Language::Kotlin], vec![BuildSystem::Gradle]),
            ],
        );
        assert!(selection.unsupported.is_empty());
        assert_eq!(selection.batches.len(), 2);
        let assignments = selection
            .batches
            .iter()
            .map(|batch| {
                (
                    batch.language.clone(),
                    batch.worker.installation.name.as_str(),
                )
            })
            .collect::<Vec<_>>();
        assert!(assignments.contains(&(Language::Kotlin, "kotlin")));
        assert!(assignments.contains(&(Language::Rust, "rust")));
    }
    #[test]
    fn leaves_missing_worker_as_structured_unsupported_batch() {
        let selection = select_discovered(
            &manifest(),
            vec![source("cargo:rust", Language::Rust, "crate/lib.rs")],
            &[WorkerCapability::FileAnalysisSnapshot],
            vec![],
        );
        assert!(selection.batches.is_empty());
        assert_eq!(selection.unsupported.len(), 1);
        assert_eq!(selection.unsupported[0].language, Language::Rust);
        assert_eq!(selection.unsupported[0].build_system, BuildSystem::Cargo);
    }
    #[test]
    fn requires_advertised_capabilities() {
        let selection = select_discovered(
            &manifest(),
            vec![source("gradle:kotlin", Language::Kotlin, "src/App.kt")],
            &[WorkerCapability::DependencyAnalysis],
            vec![worker(
                "kotlin",
                vec![Language::Kotlin],
                vec![BuildSystem::Gradle],
            )],
        );
        assert_eq!(selection.unsupported.len(), 1);
    }

    #[test]
    fn shards_large_component_language_batches_deterministically() {
        let limit = 2;
        let sources = (0..(limit + 1))
            .map(|index| source("gradle:kotlin", Language::Kotlin, &format!("src/File{index}.kt")))
            .collect::<Vec<_>>();
        let selection = select_discovered_with_batch_limit(
            &manifest(),
            sources,
            &[WorkerCapability::FileAnalysisSnapshot],
            vec![worker("kotlin", vec![Language::Kotlin], vec![BuildSystem::Gradle])],
            limit,
        );

        assert!(selection.unsupported.is_empty());
        assert_eq!(selection.batches.len(), 2);
        assert_eq!(selection.batches[0].source_units.len(), limit);
        assert_eq!(selection.batches[1].source_units.len(), 1);
        assert_eq!(selection.batches[0].source_units[0].path.as_str(), "src/File0.kt");
        assert_eq!(selection.batches[1].source_units[0].path.as_str(), "src/File2.kt");
    }

    #[test]
    fn negotiates_versioned_package_capabilities_deterministically() {
        let mut kotlin = worker(
            "kotlin",
            vec![Language::Kotlin],
            vec![BuildSystem::Gradle],
        );
        kotlin.capabilities.semantic_query_capabilities = vec![
            SemanticQueryCapability {
                name: "hierarchy.direct".to_owned(),
                version: 1,
                parameters: vec![SemanticQueryParameter {
                    name: "supertype".to_owned(),
                    ty: SemanticQueryParameterType::SymbolId,
                    required: true,
                }],
                result: SemanticQueryResultKind::CandidateSymbols,
            },
            SemanticQueryCapability {
                name: "applications.resolved_target".to_owned(),
                version: 1,
                parameters: Vec::new(),
                result: SemanticQueryResultKind::NormalizedFacts,
            },
        ];
        let requirements = vec![
            PackageCapability {
                name: "missing.optional".to_owned(),
                version: 1,
                required: false,
            },
            PackageCapability {
                name: "hierarchy.direct".to_owned(),
                version: 2,
                required: true,
            },
            PackageCapability {
                name: "applications.resolved_target".to_owned(),
                version: 1,
                required: true,
            },
        ];

        let negotiated = negotiate_query_capabilities(&requirements, &[kotlin.clone()]);

        assert_eq!(
            negotiated
                .iter()
                .map(|item| (item.name.as_str(), item.status))
                .collect::<Vec<_>>(),
            vec![
                (
                    "applications.resolved_target",
                    QueryCapabilityStatus::Supported
                ),
                (
                    "hierarchy.direct",
                    QueryCapabilityStatus::IncompatibleVersion
                ),
                (
                    "missing.optional",
                    QueryCapabilityStatus::Missing
                ),
            ]
        );
        assert_eq!(negotiated[0].providers, vec!["kotlin"]);
        assert_eq!(negotiated[1].available_versions, vec![1]);
        assert_eq!(
            query_capability_support(&negotiated),
            QueryCapabilitySupport::Unsupported
        );
        assert_eq!(
            query_capability_support(&negotiate_query_capabilities(
                &requirements[..1],
                &[]
            )),
            QueryCapabilitySupport::Partial
        );
        assert_eq!(
            query_capability_support(&negotiate_query_capabilities(
                &requirements[2..],
                &[kotlin]
            )),
            QueryCapabilitySupport::Complete
        );
    }
}
