//! Deterministic filesystem fallback for project and source-unit discovery.
//!
//! Build-specific workers may later refine the manifest, but Core can always
//! inventory Kotlin and Java files without keeping such a worker resident.

use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    BuildSystem, Component, ComponentId, DependencyEdge, Fingerprint, Language, ProjectManifest,
    Provenance, SourceOrigin, SourceUnit, SourceUnitId, WORKER_PROTOCOL_VERSION, WorkspacePath,
};
use crate::{
    input_inventory::{
        ConfigurationInput, component_context_fingerprint, component_scope, fingerprint_file,
        is_configuration_input,
    },
    workspace::{find_workspace_root, has_regular_file, workspace_id, workspace_path},
};

const EXCLUDED_DIRECTORIES: &[&str] = &[
    ".git",
    ".gradle",
    ".idea",
    ".kide",
    "build",
    "node_modules",
    "out",
    "target",
];

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("cannot inspect {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error(transparent)]
    WorkspaceRoot(#[from] crate::workspace::WorkspaceRootError),
    #[error(transparent)]
    WorkspacePath(#[from] crate::workspace::WorkspacePathError),
}

/// Results of one filesystem discovery pass. `root` is canonical; all model
/// paths are slash-separated and relative to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceDiscovery {
    pub root: PathBuf,
    pub manifest: ProjectManifest,
    pub source_units: Vec<SourceUnit>,
    pub configuration_inputs: Vec<WorkspacePath>,
    /// Individually fingerprinted inputs and the component contexts they can
    /// invalidate. Core owns this inventory; workers interpret the files.
    pub configuration_input_records: Vec<ConfigurationInput>,
    /// Workspace-relative symlinks skipped by the fallback walker.
    pub skipped_symlinks: Vec<WorkspacePath>,
}

/// Builds the generic manifest and inventory. No compiler, PSI, or build tool
/// is started by this function.
pub fn discover_workspace(
    invocation: impl AsRef<Path>,
) -> Result<WorkspaceDiscovery, DiscoveryError> {
    let root = find_workspace_root(invocation)?;
    let mut files = Vec::new();
    let mut skipped_symlinks = Vec::new();
    collect_files(&root, &root, &mut files, &mut skipped_symlinks)?;
    files.sort();
    skipped_symlinks.sort();

    let mut configuration_paths: Vec<PathBuf> = files
        .iter()
        .filter(|path| is_configuration_input(&root, path))
        .cloned()
        .collect();
    let kide_config = root.join(".kide/config.toml");
    if kide_config.is_file() {
        configuration_paths.push(kide_config);
        configuration_paths.sort();
        configuration_paths.dedup();
    }
    let configuration_inputs = configuration_paths
        .iter()
        .map(|path| workspace_path(&root, path))
        .collect::<Result<Vec<_>, _>>()?;
    let workspace_configuration = fingerprint_files(&root, &configuration_paths)?;

    let mut component_roots = component_roots(&configuration_paths)?;
    if !component_roots.contains(&root) {
        component_roots.push(root.clone());
    }
    component_roots.sort();
    component_roots.dedup();

    let mut components = component_roots
        .iter()
        .map(|component_root| make_component(&root, component_root, &workspace_configuration))
        .collect::<Result<Vec<_>, _>>()?;
    let configuration_input_records = configuration_paths
        .iter()
        .map(|path| {
            let path_in_workspace = workspace_path(&root, path)?;
            let components = component_scope(&root, path, &components);
            Ok(ConfigurationInput {
                path: path_in_workspace,
                fingerprint: fingerprint_file(path).map_err(|source| DiscoveryError::Io {
                    path: path.clone(),
                    source,
                })?,
                components,
            })
        })
        .collect::<Result<Vec<_>, DiscoveryError>>()?;
    for component in &mut components {
        component.configuration =
            component_context_fingerprint(&component.id, &configuration_input_records);
    }
    let source_units = source_units(&root, &files, &components, &component_roots)?;
    let workspace_identity = workspace_id(&root);
    let provenance = Provenance {
        backend: "kide-filesystem-discovery".to_owned(),
        backend_version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol_version: WORKER_PROTOCOL_VERSION,
        analysis_options: workspace_configuration.clone(),
    };

    Ok(WorkspaceDiscovery {
        root: root.clone(),
        manifest: ProjectManifest {
            workspace: workspace_identity,
            root: WorkspacePath::new("."),
            components,
            dependencies: Vec::<DependencyEdge>::new(),
            fingerprint: workspace_configuration,
            provenance,
        },
        source_units,
        configuration_inputs,
        configuration_input_records,
        skipped_symlinks: skipped_symlinks
            .iter()
            .map(|path| workspace_path(&root, path))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<PathBuf>,
    skipped_symlinks: &mut Vec<PathBuf>,
) -> Result<(), DiscoveryError> {
    let mut entries = fs::read_dir(directory)
        .map_err(|source| DiscoveryError::Io {
            path: directory.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| DiscoveryError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| DiscoveryError::Io {
            path: path.clone(),
            source,
        })?;
        if file_type.is_symlink() {
            skipped_symlinks.push(path);
        } else if file_type.is_dir() {
            let name = entry.file_name();
            if !EXCLUDED_DIRECTORIES
                .iter()
                .any(|excluded| name == *excluded)
            {
                collect_files(root, &path, files, skipped_symlinks)?;
            }
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    let _ = root;
    Ok(())
}

fn component_roots(configuration_paths: &[PathBuf]) -> Result<Vec<PathBuf>, DiscoveryError> {
    let roots = configuration_paths
        .iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    matches!(
                        name,
                        "build.gradle"
                            | "build.gradle.kts"
                            | "pom.xml"
                            | "Cargo.toml"
                            | "package.json"
                    )
                })
        })
        .filter_map(|path| path.parent().map(Path::to_path_buf))
        .collect::<BTreeSet<_>>();
    Ok(roots.into_iter().collect())
}

fn make_component(
    workspace_root: &Path,
    component_root: &Path,
    configuration: &Fingerprint,
) -> Result<Component, DiscoveryError> {
    let root = workspace_path(workspace_root, component_root)?;
    let build_system = build_system_for(component_root)?;
    let id_prefix = match build_system {
        BuildSystem::Gradle => "gradle",
        BuildSystem::Maven => "maven",
        BuildSystem::Cargo => "cargo",
        BuildSystem::Npm => "npm",
        BuildSystem::Bazel => "bazel",
        BuildSystem::Filesystem | BuildSystem::Other(_) => "filesystem",
    };
    let root_key = if root.as_str() == "." {
        "root"
    } else {
        root.as_str()
    };
    Ok(Component {
        id: ComponentId::new(format!("{id_prefix}:{root_key}:main")),
        name: root_key.replace('/', "-"),
        build_system,
        root,
        languages: vec![Language::Kotlin, Language::Java],
        configuration: configuration.clone(),
        source_sets: Vec::new(),
        classpath: Vec::new(),
        toolchain: None,
        compiler_configuration: None,
    })
}

fn build_system_for(root: &Path) -> Result<BuildSystem, DiscoveryError> {
    if has_regular_file(root, "build.gradle")? || has_regular_file(root, "build.gradle.kts")? {
        Ok(BuildSystem::Gradle)
    } else if has_regular_file(root, "pom.xml")? {
        Ok(BuildSystem::Maven)
    } else if has_regular_file(root, "Cargo.toml")? {
        Ok(BuildSystem::Cargo)
    } else if has_regular_file(root, "package.json")? {
        Ok(BuildSystem::Npm)
    } else {
        Ok(BuildSystem::Filesystem)
    }
}

fn source_units(
    root: &Path,
    files: &[PathBuf],
    components: &[Component],
    component_roots: &[PathBuf],
) -> Result<Vec<SourceUnit>, DiscoveryError> {
    let mut roots_and_components = component_roots
        .iter()
        .filter_map(|component_root| {
            components
                .iter()
                .find(|component| {
                    workspace_path(root, component_root).ok().as_ref() == Some(&component.root)
                })
                .map(|component| (component_root, component))
        })
        .collect::<Vec<_>>();
    roots_and_components
        .sort_by_key(|(component_root, _)| std::cmp::Reverse(component_root.components().count()));

    let fallback = components
        .iter()
        .find(|component| component.root.as_str() == ".")
        .expect("root component is always present");
    let mut source_units = files
        .iter()
        .filter_map(|path| {
            (!is_configuration_input(root, path))
                .then(|| language_for(path))
                .flatten()
                .map(|language| (path, language))
        })
        .map(|(path, language)| {
            let component = roots_and_components
                .iter()
                .find(|(component_root, _)| path.starts_with(component_root))
                .map(|(_, component)| *component)
                .unwrap_or(fallback);
            let path_in_workspace = workspace_path(root, path)?;
            let content =
                fingerprint_bytes(&fs::read(path).map_err(|source| DiscoveryError::Io {
                    path: path.clone(),
                    source,
                })?);
            let origin = if is_generated_source(&path_in_workspace) {
                SourceOrigin::Generated
            } else {
                SourceOrigin::Source
            };
            Ok(SourceUnit {
                id: SourceUnitId::new(format!(
                    "{}:{}",
                    component.id.as_str(),
                    path_in_workspace.as_str()
                )),
                component: component.id.clone(),
                path: path_in_workspace,
                language,
                origin,
                content,
                context: component.configuration.clone(),
            })
        })
        .collect::<Result<Vec<_>, DiscoveryError>>()?;
    source_units.sort_by(|left, right| left.path.as_str().cmp(right.path.as_str()));
    Ok(source_units)
}

fn language_for(path: &Path) -> Option<Language> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("kt") | Some("kts") => Some(Language::Kotlin),
        Some("java") => Some(Language::Java),
        _ => None,
    }
}

fn is_generated_source(path: &WorkspacePath) -> bool {
    path.as_str()
        .split('/')
        .any(|segment| segment == "generated")
}

fn fingerprint_files(root: &Path, paths: &[PathBuf]) -> Result<Fingerprint, DiscoveryError> {
    let mut hasher = Sha256::new();
    for path in paths {
        let normalized = workspace_path(root, path)?;
        hasher.update(normalized.as_str().as_bytes());
        hasher.update([0]);
        let contents = fs::read(path).map_err(|source| DiscoveryError::Io {
            path: path.clone(),
            source,
        })?;
        hasher.update((contents.len() as u64).to_be_bytes());
        hasher.update(contents);
    }
    Ok(Fingerprint::new(format!("sha256:{:x}", hasher.finalize())))
}

fn fingerprint_bytes(bytes: &[u8]) -> Fingerprint {
    Fingerprint::new(format!("sha256:{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn discovers_a_multi_module_workspace_from_a_nested_path_deterministically() {
        let fixture = fixture_workspace();
        let root = fixture.path();
        let nested = root.join("app/src/main/kotlin");

        let from_root = discover_workspace(root).expect("discovers root");
        let from_nested = discover_workspace(&nested).expect("discovers nested invocation");

        assert_eq!(
            from_nested.root,
            root.canonicalize().expect("canonical root")
        );
        assert_eq!(from_root, from_nested);
        assert_eq!(from_root.manifest.components.len(), 3);
        assert_eq!(
            source_paths(&from_root),
            vec![
                "app/src/main/kotlin/App.kt",
                "lib/src/main/java/Library.java",
                "src/generated/kotlin/Generated.kt",
            ]
        );
        assert!(
            from_root
                .source_units
                .iter()
                .any(|source| source.language == Language::Kotlin)
        );
        assert!(
            from_root
                .source_units
                .iter()
                .any(|source| source.language == Language::Java)
        );
        assert!(
            from_root
                .source_units
                .iter()
                .any(|source| source.origin == SourceOrigin::Generated)
        );
        assert!(
            from_root
                .configuration_inputs
                .windows(2)
                .all(|pair| pair[0].as_str() < pair[1].as_str())
        );
    }

    #[test]
    fn source_identity_and_fingerprints_are_stable_and_content_sensitive() {
        let fixture = fixture_workspace();
        let root = fixture.path();
        let before = discover_workspace(root).expect("initial discovery");
        let before_app = before
            .source_units
            .iter()
            .find(|source| source.path.as_str() == "app/src/main/kotlin/App.kt")
            .expect("app source");

        fs::write(
            root.join("app/src/main/kotlin/App.kt"),
            "class App { fun changed() = Unit }\n",
        )
        .expect("changes source");
        let after = discover_workspace(root).expect("second discovery");
        let after_app = after
            .source_units
            .iter()
            .find(|source| source.path.as_str() == "app/src/main/kotlin/App.kt")
            .expect("app source");

        assert_eq!(before_app.id, after_app.id);
        assert_ne!(before_app.content, after_app.content);
        assert_eq!(before_app.context, after_app.context);
        assert_eq!(before.manifest.fingerprint, after.manifest.fingerprint);
    }

    #[test]
    fn javascript_lockfiles_are_configuration_inputs_that_change_context() {
        let fixture = tempdir().expect("temporary fixture");
        let root = fixture.path();
        write(root.join("package.json"), "{\"name\":\"app\"}\n");
        write(root.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n");
        write(root.join("src/Main.java"), "class Main {}\n");

        let before = discover_workspace(root).expect("discovers lockfile");
        assert!(
            before
                .configuration_inputs
                .contains(&WorkspacePath::new("pnpm-lock.yaml"))
        );
        let before_context = before.source_units[0].context.clone();
        fs::write(
            root.join("pnpm-lock.yaml"),
            "lockfileVersion: '9.0'\npackages: {}\n",
        )
        .expect("updates lockfile");
        let after = discover_workspace(root).expect("rediscovers changed lockfile");
        assert_ne!(before_context, after.source_units[0].context);
        assert_ne!(before.manifest.fingerprint, after.manifest.fingerprint);
    }

    #[test]
    fn module_configuration_changes_only_that_component_context() {
        let fixture = fixture_workspace();
        let before = discover_workspace(fixture.path()).unwrap();
        let contexts = |discovery: &WorkspaceDiscovery| {
            discovery
                .source_units
                .iter()
                .map(|source| (source.path.as_str().to_owned(), source.context.clone()))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let before = contexts(&before);
        fs::write(
            fixture.path().join("app/build.gradle.kts"),
            "plugins { kotlin(\"jvm\") }\ndependencies {}\n",
        )
        .unwrap();
        let after_discovery = discover_workspace(fixture.path()).unwrap();
        let after = contexts(&after_discovery);
        assert_ne!(
            before["app/src/main/kotlin/App.kt"],
            after["app/src/main/kotlin/App.kt"]
        );
        assert_eq!(
            before["lib/src/main/java/Library.java"],
            after["lib/src/main/java/Library.java"]
        );
    }

    #[test]
    fn kide_configuration_is_workspace_wide_even_though_state_directory_is_excluded() {
        let fixture = fixture_workspace();
        write(
            fixture.path().join(".kide/config.toml"),
            "schema_version = 1\nfreshness_strategy = \"fresh_only\"\n",
        );
        let discovery = discover_workspace(fixture.path()).unwrap();
        let input = discovery
            .configuration_input_records
            .iter()
            .find(|input| input.path.as_str() == ".kide/config.toml")
            .expect("KIDE config is inventoried");
        assert_eq!(input.components.len(), discovery.manifest.components.len());
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_workspace_internal_symlinks() {
        use std::os::unix::fs::symlink;

        let fixture = fixture_workspace();
        let root = fixture.path();
        symlink(root.join("app/src"), root.join("linked-src")).expect("creates source symlink");

        let discovery = discover_workspace(root).expect("discovers workspace");

        assert_eq!(
            source_paths(&discovery)
                .iter()
                .filter(|path| path.contains("linked-src"))
                .count(),
            0
        );
        assert_eq!(
            discovery.skipped_symlinks,
            vec![WorkspacePath::new("linked-src")]
        );
    }

    fn fixture_workspace() -> tempfile::TempDir {
        let fixture = tempdir().expect("temporary fixture");
        let root = fixture.path();
        write(
            root.join("settings.gradle.kts"),
            "include(\":app\", \":lib\")\n",
        );
        write(
            root.join("gradle.properties"),
            "kotlin.code.style=official\n",
        );
        write(
            root.join("app/build.gradle.kts"),
            "plugins { kotlin(\"jvm\") }\n",
        );
        write(root.join("app/src/main/kotlin/App.kt"), "class App\n");
        write(root.join("lib/build.gradle.kts"), "plugins { java }\n");
        write(
            root.join("lib/src/main/java/Library.java"),
            "class Library {}\n",
        );
        write(
            root.join("src/generated/kotlin/Generated.kt"),
            "class Generated\n",
        );
        write(
            root.join("build/generated/kotlin/Ignored.kt"),
            "class Ignored\n",
        );
        write(root.join(".idea/Ignored.java"), "class Ignored {}\n");
        fixture
    }

    fn write(path: PathBuf, contents: &str) {
        fs::create_dir_all(path.parent().expect("parent")).expect("creates parent");
        fs::write(path, contents).expect("writes fixture file");
    }

    fn source_paths(discovery: &WorkspaceDiscovery) -> Vec<&str> {
        discovery
            .source_units
            .iter()
            .map(|source| source.path.as_str())
            .collect()
    }
}
