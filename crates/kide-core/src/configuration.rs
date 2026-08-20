//! Versioned, layered workspace configuration.
//!
//! Configuration is an input to semantic freshness, not a display preference.
//! The effective value is always the built-in defaults overlaid by an optional
//! global file and then `.kide/config.toml` in the workspace.  A missing file
//! is normal; an existing invalid file is an error and is never ignored.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The configuration schema understood by this Core release.
pub const CONFIGURATION_SCHEMA_VERSION: u32 = 1;

/// How semantic queries handle facts whose owners no longer match their
/// indexed input fingerprint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessStrategy {
    /// Return no semantic payload when an owning input is not verified.
    #[default]
    FreshOnly,
    /// Return the payload, but make the stale status and metadata explicit.
    AllowStale,
}

/// Typed effective configuration passed from the CLI into Core services.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectiveConfiguration {
    pub schema_version: u32,
    pub freshness_strategy: FreshnessStrategy,
}

impl Default for EffectiveConfiguration {
    fn default() -> Self {
        Self {
            schema_version: CONFIGURATION_SCHEMA_VERSION,
            freshness_strategy: FreshnessStrategy::FreshOnly,
        }
    }
}

/// Input locations that contributed to an effective configuration.  The
/// status surface may expose these workspace-relative roles but never needs to
/// reveal a user's home directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigurationSources {
    pub global: Option<PathBuf>,
    pub workspace: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConfiguration {
    pub effective: EffectiveConfiguration,
    pub sources: ConfigurationSources,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigurationLayer {
    schema_version: Option<u32>,
    freshness_strategy: Option<FreshnessStrategy>,
}

#[derive(Debug, Error)]
pub enum ConfigurationError {
    #[error("failed to read KIDE configuration `{path}`: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("failed to parse KIDE configuration `{path}`: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error(
        "KIDE configuration `{path}` uses unsupported schema version {found}; supported version is {supported}"
    )]
    UnsupportedSchema {
        path: PathBuf,
        found: u32,
        supported: u32,
    },
    #[error("failed to create KIDE configuration directory `{path}`: {source}")]
    CreateDirectory { path: PathBuf, source: io::Error },
    #[error("KIDE configuration already exists at `{0}`")]
    AlreadyInitialized(PathBuf),
    #[error("failed to write KIDE configuration `{path}`: {source}")]
    Write { path: PathBuf, source: io::Error },
}

/// The deterministic starter file written by `kide init`.
pub const WORKSPACE_CONFIGURATION_TEMPLATE: &str = "# KIDE workspace configuration schema.\n# Workspace values override the optional global file selected by KIDE_GLOBAL_CONFIG.\nschema_version = 1\n\n# Do not serve semantic answers whose source owners changed after indexing.\n# Set to \"allow_stale\" only when callers explicitly handle `status: stale`.\nfreshness_strategy = \"fresh_only\"\n";

/// The local ignore policy installed beside the checked-in configuration.
/// Git applies this file automatically to `.kide` contents; root `.gitignore`
/// only needs to make this policy file and `config.toml` reachable.
pub const WORKSPACE_GITIGNORE_TEMPLATE: &str =
    "# Persistent KIDE state is local and reproducible.\n*\n!.gitignore\n!config.toml\n";

/// Loads built-in defaults, then `global_path`, then the workspace override.
/// Later layers override only the fields they state, so a workspace can
/// override freshness without copying the global configuration.
pub fn load_configuration(
    workspace_root: &Path,
    global_path: Option<&Path>,
) -> Result<LoadedConfiguration, ConfigurationError> {
    let mut effective = EffectiveConfiguration::default();
    let mut sources = ConfigurationSources::default();
    if let Some(path) = global_path.filter(|path| path.is_file()) {
        apply_layer(&mut effective, path)?;
        sources.global = Some(path.to_path_buf());
    }
    let workspace_path = workspace_root.join(".kide/config.toml");
    if workspace_path.is_file() {
        apply_layer(&mut effective, &workspace_path)?;
        sources.workspace = Some(workspace_path);
    }
    Ok(LoadedConfiguration { effective, sources })
}

/// Loads the optional user-selected global file from `KIDE_GLOBAL_CONFIG`.
/// It intentionally has no implicit home-directory path, making test and
/// daemon environments deterministic.
pub fn load_workspace_configuration(
    workspace_root: &Path,
) -> Result<LoadedConfiguration, ConfigurationError> {
    let global = std::env::var_os("KIDE_GLOBAL_CONFIG").map(PathBuf::from);
    load_configuration(workspace_root, global.as_deref())
}

/// Creates the checked-in workspace configuration without overwriting an
/// existing file. Initialisation is deliberately explicit so indexing never
/// mutates a user's working tree as a surprise side effect.
pub fn initialize_workspace_configuration(
    workspace_root: &Path,
) -> Result<PathBuf, ConfigurationError> {
    let directory = workspace_root.join(".kide");
    let path = directory.join("config.toml");
    if path.exists() {
        return Err(ConfigurationError::AlreadyInitialized(path));
    }
    fs::create_dir_all(&directory).map_err(|source| ConfigurationError::CreateDirectory {
        path: directory.clone(),
        source,
    })?;
    fs::write(&path, WORKSPACE_CONFIGURATION_TEMPLATE).map_err(|source| {
        ConfigurationError::Write {
            path: path.clone(),
            source,
        }
    })?;
    let gitignore = directory.join(".gitignore");
    fs::write(&gitignore, WORKSPACE_GITIGNORE_TEMPLATE).map_err(|source| {
        ConfigurationError::Write {
            path: gitignore,
            source,
        }
    })?;
    Ok(path)
}

fn apply_layer(
    effective: &mut EffectiveConfiguration,
    path: &Path,
) -> Result<(), ConfigurationError> {
    let text = fs::read_to_string(path).map_err(|source| ConfigurationError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let layer: ConfigurationLayer =
        toml::from_str(&text).map_err(|source| ConfigurationError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    if let Some(found) = layer.schema_version
        && found != CONFIGURATION_SCHEMA_VERSION
    {
        return Err(ConfigurationError::UnsupportedSchema {
            path: path.to_path_buf(),
            found,
            supported: CONFIGURATION_SCHEMA_VERSION,
        });
    }
    if let Some(strategy) = layer.freshness_strategy {
        effective.freshness_strategy = strategy;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn defaults_to_fresh_only_without_files() {
        let workspace = tempdir().unwrap();
        let loaded = load_configuration(workspace.path(), None).unwrap();
        assert_eq!(loaded.effective, EffectiveConfiguration::default());
        assert_eq!(loaded.sources, ConfigurationSources::default());
    }

    #[test]
    fn workspace_overrides_global_field_by_field() {
        let workspace = tempdir().unwrap();
        let global = workspace.path().join("global.toml");
        fs::write(
            &global,
            "schema_version = 1\nfreshness_strategy = 'allow_stale'\n",
        )
        .unwrap();
        fs::create_dir(workspace.path().join(".kide")).unwrap();
        fs::write(
            workspace.path().join(".kide/config.toml"),
            "schema_version = 1\nfreshness_strategy = 'fresh_only'\n",
        )
        .unwrap();

        let loaded = load_configuration(workspace.path(), Some(&global)).unwrap();
        assert_eq!(
            loaded.effective.freshness_strategy,
            FreshnessStrategy::FreshOnly
        );
        assert_eq!(loaded.sources.global, Some(global));
        assert_eq!(
            loaded.sources.workspace,
            Some(workspace.path().join(".kide/config.toml"))
        );
    }

    #[test]
    fn rejects_unknown_keys_invalid_values_and_newer_schema() {
        let workspace = tempdir().unwrap();
        let config = workspace.path().join("config.toml");
        fs::write(&config, "surprise = true\n").unwrap();
        assert!(matches!(
            load_configuration(workspace.path(), Some(&config)),
            Err(ConfigurationError::Parse { .. })
        ));
        fs::write(&config, "freshness_strategy = 'eventually'\n").unwrap();
        assert!(matches!(
            load_configuration(workspace.path(), Some(&config)),
            Err(ConfigurationError::Parse { .. })
        ));
        fs::write(&config, "schema_version = 2\n").unwrap();
        assert!(matches!(
            load_configuration(workspace.path(), Some(&config)),
            Err(ConfigurationError::UnsupportedSchema { found: 2, .. })
        ));
    }

    #[test]
    fn initialization_writes_the_safe_template_once() {
        let workspace = tempdir().unwrap();
        let path = initialize_workspace_configuration(workspace.path()).unwrap();
        let loaded = load_configuration(workspace.path(), None).unwrap();
        assert_eq!(path, workspace.path().join(".kide/config.toml"));
        assert_eq!(
            loaded.effective.freshness_strategy,
            FreshnessStrategy::FreshOnly
        );
        assert_eq!(
            fs::read_to_string(workspace.path().join(".kide/.gitignore")).unwrap(),
            WORKSPACE_GITIGNORE_TEMPLATE
        );
        assert!(matches!(
            initialize_workspace_configuration(workspace.path()),
            Err(ConfigurationError::AlreadyInitialized(existing)) if existing == path
        ));
    }
}
