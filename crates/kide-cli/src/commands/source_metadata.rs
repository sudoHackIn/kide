//! Shared cheap source identity checks for index and status.

use std::{collections::BTreeMap, fs, io, path::Path, time::UNIX_EPOCH};

use kide_core::{Fingerprint, SourceFileMetadata, WorkspacePath};
use sha2::{Digest, Sha256};

/// Returns a source fingerprint, reusing the persisted SHA-256 only after the
/// file's size and modification timestamp both match.
pub(super) fn observe_source_file(
    workspace: &Path,
    path: &Path,
    cached: &BTreeMap<String, SourceFileMetadata>,
) -> Result<SourceFileMetadata, io::Error> {
    let key = path
        .strip_prefix(workspace)
        .expect("workspace path")
        .to_string_lossy()
        .replace('\\', "/");
    let metadata = fs::metadata(path)?;
    let modified_nanos = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let modified_nanos = i128::try_from(modified_nanos).map_err(io::Error::other)?;
    let byte_size = i64::try_from(metadata.len()).map_err(io::Error::other)?;
    let content = if let Some(previous) = cached.get(&key).filter(|previous| {
        previous.byte_size == byte_size && previous.modified_nanos == modified_nanos
    }) {
        previous.content.clone()
    } else {
        Fingerprint::new(format!("sha256:{:x}", Sha256::digest(fs::read(path)?)))
    };
    Ok(SourceFileMetadata {
        path: WorkspacePath::new(key),
        byte_size,
        modified_nanos,
        content,
    })
}
