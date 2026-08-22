//! Recognition rules for workspace configuration and dependency-resolution inputs.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::Path,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Component, ComponentId, Fingerprint, WorkspacePath};

/// One individually verifiable configuration or dependency-resolution input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigurationInput {
    pub path: WorkspacePath,
    pub fingerprint: Fingerprint,
    pub components: Vec<ComponentId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationInputState {
    Current,
    Added,
    Changed,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigurationInputStatus {
    pub path: WorkspacePath,
    pub state: ConfigurationInputState,
    pub persisted_fingerprint: Option<Fingerprint>,
    pub current_fingerprint: Option<Fingerprint>,
    pub components: Vec<ComponentId>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigurationInputReconciliation {
    pub inputs: Vec<ConfigurationInputStatus>,
    pub affected_components: Vec<ComponentId>,
}

pub(crate) const CONFIGURATION_FILE_NAMES: &[&str] = &[
    "settings.gradle",
    "settings.gradle.kts",
    "build.gradle",
    "build.gradle.kts",
    "gradle.properties",
    "pom.xml",
    "Cargo.toml",
    "package.json",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "pnpm-workspace.yaml",
    "yarn.lock",
    ".yarnrc.yml",
    "bun.lock",
    "bun.lockb",
    ".npmrc",
];

pub(crate) fn is_configuration_input(root: &Path, path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| CONFIGURATION_FILE_NAMES.contains(&name))
        || path.strip_prefix(root).ok().is_some_and(|relative| {
            relative == Path::new("gradle/wrapper/gradle-wrapper.properties")
                || relative == Path::new(".kide/config.toml")
        })
}

pub(crate) fn component_scope(
    root: &Path,
    path: &Path,
    components: &[Component],
) -> Vec<ComponentId> {
    if path.parent() == Some(root)
        || path.strip_prefix(root).ok() == Some(Path::new(".kide/config.toml"))
    {
        return components
            .iter()
            .map(|component| component.id.clone())
            .collect();
    }
    let deepest = components
        .iter()
        .filter(|component| path.starts_with(root.join(component.root.as_str())))
        .map(|component| component.root.as_str().len())
        .max()
        .unwrap_or_default();
    components
        .iter()
        .filter(|component| {
            path.starts_with(root.join(component.root.as_str()))
                && component.root.as_str().len() == deepest
        })
        .map(|component| component.id.clone())
        .collect()
}

/// Content identity of one tracked input. The path is stored separately, so a
/// rename is observable even when file contents are unchanged.
pub(crate) fn fingerprint_file(path: &Path) -> Result<Fingerprint, io::Error> {
    Ok(Fingerprint::new(format!(
        "sha256:{:x}",
        Sha256::digest(fs::read(path)?)
    )))
}

pub fn reconcile_configuration_inputs(
    persisted: &[ConfigurationInput],
    current: &[ConfigurationInput],
) -> ConfigurationInputReconciliation {
    let persisted = persisted
        .iter()
        .map(|input| (input.path.as_str(), input))
        .collect::<BTreeMap<_, _>>();
    let current = current
        .iter()
        .map(|input| (input.path.as_str(), input))
        .collect::<BTreeMap<_, _>>();
    let paths = persisted
        .keys()
        .chain(current.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    let mut affected = BTreeSet::new();
    let inputs = paths
        .into_iter()
        .map(|path| {
            let previous = persisted.get(path).copied();
            let next = current.get(path).copied();
            let state = match (previous, next) {
                (None, Some(_)) => ConfigurationInputState::Added,
                (Some(_), None) => ConfigurationInputState::Missing,
                (Some(previous), Some(next))
                    if previous.fingerprint != next.fingerprint
                        || previous.components != next.components =>
                {
                    ConfigurationInputState::Changed
                }
                (Some(_), Some(_)) => ConfigurationInputState::Current,
                (None, None) => unreachable!(),
            };
            let components = next
                .or(previous)
                .map(|input| input.components.clone())
                .unwrap_or_default();
            if state != ConfigurationInputState::Current {
                affected.extend(
                    components
                        .iter()
                        .map(|component| component.as_str().to_owned()),
                );
            }
            ConfigurationInputStatus {
                path: WorkspacePath::new(path),
                state,
                persisted_fingerprint: previous.map(|input| input.fingerprint.clone()),
                current_fingerprint: next.map(|input| input.fingerprint.clone()),
                components,
            }
        })
        .collect();
    ConfigurationInputReconciliation {
        inputs,
        affected_components: affected.into_iter().map(ComponentId::new).collect(),
    }
}

pub(crate) fn component_context_fingerprint(
    component: &ComponentId,
    inputs: &[ConfigurationInput],
) -> Fingerprint {
    let mut hasher = Sha256::new();
    for input in inputs
        .iter()
        .filter(|input| input.components.contains(component))
    {
        hasher.update(input.path.as_str().as_bytes());
        hasher.update([0]);
        hasher.update(input.fingerprint.as_str().as_bytes());
        hasher.update([0]);
    }
    Fingerprint::new(format!("sha256:{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(path: &str, digest: &str, component: &str) -> ConfigurationInput {
        ConfigurationInput {
            path: WorkspacePath::new(path),
            fingerprint: Fingerprint::new(digest),
            components: vec![ComponentId::new(component)],
        }
    }

    #[test]
    fn reconciliation_is_sorted_and_reports_affected_components() {
        let result = reconcile_configuration_inputs(
            &[input("deleted", "old", "b"), input("same", "same", "a")],
            &[input("added", "new", "c"), input("same", "same", "a")],
        );
        assert_eq!(
            result
                .inputs
                .iter()
                .map(|input| (input.path.as_str(), input.state))
                .collect::<Vec<_>>(),
            vec![
                ("added", ConfigurationInputState::Added),
                ("deleted", ConfigurationInputState::Missing),
                ("same", ConfigurationInputState::Current),
            ]
        );
        assert_eq!(
            result.affected_components,
            vec![ComponentId::new("b"), ComponentId::new("c")]
        );
    }
}
