//! Declarative semantic-query package registry.
//!
//! A package is data, never an executable extension point.  This module only
//! discovers and validates manifests; parsing package DSL bodies and project
//! commands is intentionally a later frontend layer.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::Fingerprint;

/// The only manifest format understood by this Core version.
pub const PACKAGE_MANIFEST_FORMAT: u32 = 1;
/// The declarative package API version supported by this Core version.
pub const PACKAGE_CORE_API_VERSION: &str = "^1";

/// The complete declarative identity and export surface of a query package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    pub format: u32,
    pub id: String,
    pub version: String,
    pub requires_core: String,
    pub exports: PackageExports,
    #[serde(default)]
    pub capabilities: Vec<PackageCapability>,
}

/// Package-visible macro and command names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageExports {
    #[serde(default)]
    pub macros: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
}

/// A declared semantic capability. It is descriptive only: it cannot load
/// code or make a worker call by itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageCapability {
    pub name: String,
    pub required: bool,
}

/// Where a registered package was obtained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageSource {
    BuiltIn,
    Workspace { manifest_path: PathBuf },
}

/// Validated package data retained for plan provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredPackage {
    pub manifest: PackageManifest,
    pub manifest_digest: Fingerprint,
    pub source: PackageSource,
}

/// The result of resolving a package-qualified macro or command name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPackageExport<'a> {
    pub package: &'a RegisteredPackage,
    pub export_name: String,
}

/// Deterministic registry of package identities. `BTreeMap` makes discovery
/// order irrelevant to later resolution and response provenance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageRegistry {
    packages: BTreeMap<String, RegisteredPackage>,
}

#[derive(Debug, Error)]
pub enum PackageRegistryError {
    #[error("failed to read query package directory `{path}`: {source}")]
    ReadDirectory { path: PathBuf, source: io::Error },
    #[error("failed to read query package manifest `{path}`: {source}")]
    ReadManifest { path: PathBuf, source: io::Error },
    #[error("query package directory `{path}` is missing package.toml")]
    MissingManifest { path: PathBuf },
    #[error("failed to parse query package manifest `{path}`: {source}")]
    ParseManifest {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("query package manifest `{path}` is invalid: {reason}")]
    InvalidManifest { path: PathBuf, reason: String },
    #[error("query package directory `{directory}` must match manifest id `{id}")]
    DirectoryIdMismatch { directory: PathBuf, id: String },
    #[error("query package `{0}` is already registered")]
    DuplicatePackage(String),
    #[error("unknown query package export `{0}`")]
    UnknownExport(String),
}

impl PackageRegistry {
    /// Loads immediate package directories under
    /// `.kide/query-packages/<package-id>/`. A missing root simply means that
    /// the workspace provides no packages.
    pub fn load_workspace(workspace_root: &Path) -> Result<Self, PackageRegistryError> {
        Self::load_workspace_with_builtins(workspace_root, std::iter::empty())
    }

    /// Loads workspace packages after the supplied built-ins. A workspace
    /// package cannot replace a built-in identity.
    pub fn load_workspace_with_builtins(
        workspace_root: &Path,
        builtins: impl IntoIterator<Item = PackageManifest>,
    ) -> Result<Self, PackageRegistryError> {
        let mut registry = Self::default();
        for manifest in builtins {
            registry.register_builtin(manifest)?;
        }
        registry.load_workspace_into(workspace_root)?;
        Ok(registry)
    }

    fn load_workspace_into(&mut self, workspace_root: &Path) -> Result<(), PackageRegistryError> {
        let packages_root = workspace_root.join(".kide/query-packages");
        if !packages_root.exists() {
            return Ok(());
        }

        let mut directories = fs::read_dir(&packages_root)
            .map_err(|source| PackageRegistryError::ReadDirectory {
                path: packages_root.clone(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| PackageRegistryError::ReadDirectory {
                path: packages_root.clone(),
                source,
            })?
            .into_iter()
            .filter_map(|entry| {
                entry
                    .file_type()
                    .ok()
                    .filter(|kind| kind.is_dir())
                    .map(|_| entry.path())
            })
            .collect::<Vec<_>>();
        directories.sort();

        for directory in directories {
            let manifest_path = directory.join("package.toml");
            if !manifest_path.is_file() {
                return Err(PackageRegistryError::MissingManifest { path: directory });
            }
            let text = fs::read_to_string(&manifest_path).map_err(|source| {
                PackageRegistryError::ReadManifest {
                    path: manifest_path.clone(),
                    source,
                }
            })?;
            let manifest =
                toml::from_str(&text).map_err(|source| PackageRegistryError::ParseManifest {
                    path: manifest_path.clone(),
                    source,
                })?;
            validate_manifest(&manifest).map_err(|reason| {
                PackageRegistryError::InvalidManifest {
                    path: manifest_path.clone(),
                    reason,
                }
            })?;
            let directory_name = directory.file_name().and_then(|name| name.to_str());
            if directory_name != Some(manifest.id.as_str()) {
                return Err(PackageRegistryError::DirectoryIdMismatch {
                    directory,
                    id: manifest.id,
                });
            }
            self.insert(RegisteredPackage {
                manifest: manifest.clone(),
                manifest_digest: canonical_manifest_digest(&manifest),
                source: PackageSource::Workspace { manifest_path },
            })?;
        }
        Ok(())
    }

    /// Registers a distribution-provided manifest after applying the same
    /// validation as a workspace package.
    pub fn register_builtin(
        &mut self,
        manifest: PackageManifest,
    ) -> Result<(), PackageRegistryError> {
        validate_manifest(&manifest).map_err(|reason| PackageRegistryError::InvalidManifest {
            path: PathBuf::from("<builtin>"),
            reason,
        })?;
        self.insert(RegisteredPackage {
            manifest: manifest.clone(),
            manifest_digest: canonical_manifest_digest(&manifest),
            source: PackageSource::BuiltIn,
        })
    }

    pub fn package(&self, id: &str) -> Option<&RegisteredPackage> {
        self.packages.get(id)
    }

    pub fn packages(&self) -> impl ExactSizeIterator<Item = (&str, &RegisteredPackage)> {
        self.packages
            .iter()
            .map(|(id, package)| (id.as_str(), package))
    }

    pub fn resolve_macro(
        &self,
        name: &str,
    ) -> Result<ResolvedPackageExport<'_>, PackageRegistryError> {
        self.resolve_export(name, |exports, candidate| {
            exports.macros.iter().any(|value| value == candidate)
        })
    }

    pub fn resolve_command(
        &self,
        name: &str,
    ) -> Result<ResolvedPackageExport<'_>, PackageRegistryError> {
        self.resolve_export(name, |exports, candidate| {
            exports.commands.iter().any(|value| value == candidate)
        })
    }

    fn insert(&mut self, package: RegisteredPackage) -> Result<(), PackageRegistryError> {
        let id = package.manifest.id.clone();
        if self.packages.insert(id.clone(), package).is_some() {
            return Err(PackageRegistryError::DuplicatePackage(id));
        }
        Ok(())
    }

    fn resolve_export(
        &self,
        name: &str,
        matches: impl Fn(&PackageExports, &str) -> bool,
    ) -> Result<ResolvedPackageExport<'_>, PackageRegistryError> {
        let mut candidates = self
            .packages
            .iter()
            .filter_map(|(id, package)| {
                name.strip_prefix(id)
                    .and_then(|suffix| suffix.strip_prefix('.'))
                    .filter(|suffix| {
                        !suffix.is_empty() && matches(&package.manifest.exports, suffix)
                    })
                    .map(|suffix| ResolvedPackageExport {
                        package,
                        export_name: suffix.to_owned(),
                    })
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .package
                .manifest
                .id
                .len()
                .cmp(&left.package.manifest.id.len())
        });
        candidates
            .into_iter()
            .next()
            .ok_or_else(|| PackageRegistryError::UnknownExport(name.to_owned()))
    }
}

fn validate_manifest(manifest: &PackageManifest) -> Result<(), String> {
    if manifest.format != PACKAGE_MANIFEST_FORMAT {
        return Err(format!("format must be {PACKAGE_MANIFEST_FORMAT}"));
    }
    if manifest.requires_core != PACKAGE_CORE_API_VERSION {
        return Err(format!(
            "requires_core must be `{PACKAGE_CORE_API_VERSION}`"
        ));
    }
    if !is_dotted_name(&manifest.id) {
        return Err("id must be an ASCII lower-case dotted name".to_owned());
    }
    if !is_semver(&manifest.version) {
        return Err("version must be a semantic version such as `1.0.0`".to_owned());
    }
    validate_names("exports.macros", &manifest.exports.macros)?;
    validate_names("exports.commands", &manifest.exports.commands)?;
    let mut capabilities = BTreeSet::new();
    for capability in &manifest.capabilities {
        if !is_capability_name(&capability.name) {
            return Err(format!(
                "capability `{}` must be an ASCII lower-case dotted name",
                capability.name
            ));
        }
        if !capabilities.insert(&capability.name) {
            return Err(format!(
                "capability `{}` is declared more than once",
                capability.name
            ));
        }
    }
    Ok(())
}

fn validate_names(label: &str, names: &[String]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for name in names {
        if !is_dotted_name(name) {
            return Err(format!(
                "{label} name `{name}` must be an ASCII lower-case dotted name"
            ));
        }
        if !seen.insert(name) {
            return Err(format!("{label} name `{name}` is declared more than once"));
        }
    }
    Ok(())
}

fn is_dotted_name(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

fn is_capability_name(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|segment| {
            !segment.is_empty()
                && segment.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'-'
                        || byte == b'_'
                })
        })
}

fn is_semver(value: &str) -> bool {
    let mut parts = value.split('.');
    parts.clone().count() == 3
        && parts.all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn canonical_manifest_digest(manifest: &PackageManifest) -> Fingerprint {
    // Struct field order and sorted registry-owned data make this independent
    // of comments, whitespace, and TOML table ordering in the source file.
    let canonical = toml::to_string(manifest).expect("package manifest always serializes");
    Fingerprint::new(format!("sha256:{:x}", Sha256::digest(canonical.as_bytes())))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    const SPRING_WEB: &str = r#"
format = 1
id = "spring.web"
version = "1.0.0"
requires_core = "^1"

[exports]
macros = ["controller"]
commands = ["controllers"]

[[capabilities]]
name = "applications.resolved_target"
required = true
"#;

    #[test]
    fn loads_workspace_packages_and_resolves_qualified_exports() {
        let workspace = tempdir().expect("temporary workspace");
        let package = workspace.path().join(".kide/query-packages/spring.web");
        fs::create_dir_all(&package).expect("package directory");
        fs::write(package.join("package.toml"), SPRING_WEB).expect("manifest");

        let registry = PackageRegistry::load_workspace(workspace.path()).expect("loads registry");
        let resolved = registry
            .resolve_macro("spring.web.controller")
            .expect("resolves exported macro");

        assert_eq!(resolved.package.manifest.version, "1.0.0");
        assert_eq!(resolved.export_name, "controller");
        assert!(resolved
            .package
            .manifest_digest
            .as_str()
            .starts_with("sha256:"));
        assert!(matches!(
            resolved.package.source,
            PackageSource::Workspace { .. }
        ));
        assert!(registry.resolve_command("spring.web.controllers").is_ok());
    }

    #[test]
    fn manifest_digest_ignores_toml_whitespace() {
        let first: PackageManifest = toml::from_str(SPRING_WEB).expect("manifest");
        let second: PackageManifest =
            toml::from_str(&SPRING_WEB.replace("\n", "\n\n")).expect("manifest");
        assert_eq!(
            canonical_manifest_digest(&first),
            canonical_manifest_digest(&second)
        );
    }

    #[test]
    fn rejects_incompatible_or_invalid_workspace_manifests() {
        let workspace = tempdir().expect("temporary workspace");
        let package = workspace.path().join(".kide/query-packages/spring.web");
        fs::create_dir_all(&package).expect("package directory");
        fs::write(
            package.join("package.toml"),
            SPRING_WEB.replace("requires_core = \"^1\"", "requires_core = \"^2\""),
        )
        .expect("manifest");

        let error = PackageRegistry::load_workspace(workspace.path())
            .expect_err("rejects incompatible manifest");
        assert!(matches!(
            error,
            PackageRegistryError::InvalidManifest { .. }
        ));
        assert!(error.to_string().contains("requires_core must be `^1`"));
    }

    #[test]
    fn rejects_package_directory_without_manifest() {
        let workspace = tempdir().expect("temporary workspace");
        fs::create_dir_all(workspace.path().join(".kide/query-packages/empty"))
            .expect("package directory");

        assert!(matches!(
            PackageRegistry::load_workspace(workspace.path()),
            Err(PackageRegistryError::MissingManifest { .. })
        ));
    }

    #[test]
    fn workspace_cannot_replace_a_builtin_package() {
        let workspace = tempdir().expect("temporary workspace");
        let package = workspace.path().join(".kide/query-packages/spring.web");
        fs::create_dir_all(&package).expect("package directory");
        fs::write(package.join("package.toml"), SPRING_WEB).expect("manifest");
        let builtin: PackageManifest = toml::from_str(SPRING_WEB).expect("builtin manifest");

        assert!(matches!(
            PackageRegistry::load_workspace_with_builtins(workspace.path(), [builtin]),
            Err(PackageRegistryError::DuplicatePackage(id)) if id == "spring.web"
        ));
    }
}
