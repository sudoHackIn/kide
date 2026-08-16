//! Discovery and selection of disposable workers.
//!
//! The registry is deliberately a Core concern: it maps a locally installed
//! executable to the build contexts it can serve, then trusts the worker's
//! handshake for its language and protocol capabilities.  Launch arguments
//! remain process-local and never enter the canonical worker protocol.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::{
    BuildSystem, ComponentId, Language, ProjectManifest, SourceUnit, WorkerCapabilities,
    WorkerCapability, WorkerLaunch, WorkerSupervisor, WorkerSupervisorError,
};

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
        let discovered = self.discover()?;
        Ok(select_discovered(manifest, sources, required, discovered))
    }
}

pub fn select_discovered(
    manifest: &ProjectManifest,
    mut sources: Vec<SourceUnit>,
    required: &[WorkerCapability],
    mut workers: Vec<DiscoveredWorker>,
) -> WorkerSelection {
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
            Some(worker) => selection.batches.push(WorkerBatch {
                worker: worker.clone(),
                component,
                language,
                source_units,
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
        WorkerIdentity, WorkspaceId, WorkspacePath, WORKER_PROTOCOL_VERSION,
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
}
