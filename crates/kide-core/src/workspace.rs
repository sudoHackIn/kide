//! Workspace root, identity, and path primitives.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use crate::{WorkspaceId, WorkspacePath, input_inventory::CONFIGURATION_FILE_NAMES};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkspaceRootError {
    #[error("cannot inspect {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{path} is neither a regular file nor a directory")]
    UnsupportedInvocationPath { path: PathBuf },
}

#[derive(Debug, Error)]
#[error("{path} escapes workspace root {workspace_root}")]
pub struct WorkspacePathError {
    pub path: PathBuf,
    pub workspace_root: PathBuf,
}

pub(crate) fn workspace_id(root: &Path) -> WorkspaceId {
    WorkspaceId::new(format!(
        "filesystem:sha256:{:x}",
        Sha256::digest(root.to_string_lossy().as_bytes())
    ))
}

pub(crate) fn workspace_path(root: &Path, path: &Path) -> Result<WorkspacePath, WorkspacePathError> {
    let relative = path.strip_prefix(root).map_err(|_| WorkspacePathError {
        path: path.to_path_buf(), workspace_root: root.to_path_buf(),
    })?;
    let text = relative.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
    Ok(WorkspacePath::new(if text.is_empty() { "." } else { &text }))
}

/// Resolves an invocation path to the closest authoritative workspace marker.
/// Gradle settings win over a nested build manifest; a build manifest wins
/// over an enclosing Git repository.
pub fn find_workspace_root(
    invocation: impl AsRef<Path>,
) -> Result<PathBuf, WorkspaceRootError> {
    let invocation = invocation.as_ref();
    let canonical = fs::canonicalize(invocation).map_err(|source| WorkspaceRootError::Io {
        path: invocation.to_path_buf(),
        source,
    })?;
    let metadata = fs::metadata(&canonical).map_err(|source| WorkspaceRootError::Io {
        path: canonical.clone(),
        source,
    })?;
    let start = if metadata.is_file() {
        canonical
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| WorkspaceRootError::UnsupportedInvocationPath {
                path: canonical.clone(),
            })?
    } else if metadata.is_dir() {
        canonical
    } else {
        return Err(WorkspaceRootError::UnsupportedInvocationPath { path: canonical });
    };

    let mut settings_root = None;
    let mut git_root = None;
    let mut build_root = None;
    for directory in start.ancestors() {
        if settings_root.is_none()
            && (has_regular_file(directory, "settings.gradle")?
                || has_regular_file(directory, "settings.gradle.kts")?)
        {
            settings_root = Some(directory.to_path_buf());
        }
        if git_root.is_none() && has_directory(directory, ".git")? {
            git_root = Some(directory.to_path_buf());
        }
        if build_root.is_none() {
            for name in CONFIGURATION_FILE_NAMES {
                if has_regular_file(directory, name)? {
                    build_root = Some(directory.to_path_buf());
                    break;
                }
            }
        }
    }
    Ok(settings_root.or(build_root).or(git_root).unwrap_or(start))
}

pub(crate) fn has_regular_file(
    directory: &Path,
    name: &str,
) -> Result<bool, WorkspaceRootError> {
    file_type(directory, name).map(|kind| kind.is_some_and(|kind| kind.is_file()))
}

fn has_directory(directory: &Path, name: &str) -> Result<bool, WorkspaceRootError> {
    file_type(directory, name).map(|kind| kind.is_some_and(|kind| kind.is_dir()))
}

fn file_type(
    directory: &Path,
    name: &str,
) -> Result<Option<fs::FileType>, WorkspaceRootError> {
    let path = directory.join(name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => Ok(Some(metadata.file_type())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(WorkspaceRootError::Io { path, source }),
    }
}
