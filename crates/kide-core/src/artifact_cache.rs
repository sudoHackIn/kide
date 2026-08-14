//! Shared, immutable storage for dependency artifact payloads.
//!
//! This cache intentionally has no SQLite dependency. A project index records
//! only a cache key; callers may load or publish blobs in parallel with index
//! transactions. Publication uses a temporary file plus an atomic hard-link,
//! so competing workers never observe a partial blob.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{CANONICAL_SCHEMA_VERSION, Fingerprint, Provenance};

const MAGIC: [u8; 8] = *b"KIDEBLB1";
const HEADER_SIZE: usize = MAGIC.len() + 4 + 32 + 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactBlobKey {
    artifact: Fingerprint,
    backend: String,
    backend_version: String,
    protocol_version: u32,
    canonical_schema_version: u32,
}

impl ArtifactBlobKey {
    pub fn new(artifact: Fingerprint, provenance: &Provenance) -> Self {
        Self {
            artifact,
            backend: provenance.backend.clone(),
            backend_version: provenance.backend_version.clone(),
            protocol_version: provenance.protocol_version,
            canonical_schema_version: CANONICAL_SCHEMA_VERSION,
        }
    }

    fn digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        for field in [
            self.artifact.as_str(),
            &self.backend,
            &self.backend_version,
            &self.protocol_version.to_string(),
            &self.canonical_schema_version.to_string(),
        ] {
            hasher.update(field.as_bytes());
            hasher.update([0]);
        }
        hasher.finalize().into()
    }
}

#[derive(Debug, Error)]
pub enum ArtifactBlobCacheError {
    #[error("artifact blob cache I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("artifact blob has an incompatible or corrupt header")]
    InvalidHeader,
}

/// A content-addressed cache rooted outside an individual project index.
///
/// The caller supplies the root deliberately, allowing the CLI to select one
/// user-level shared directory while tests and embedders can use an isolated
/// cache. Payload bytes are opaque to this type: no JSON encoding or decoding
/// occurs on the blob path.
#[derive(Debug, Clone)]
pub struct ArtifactBlobCache {
    root: PathBuf,
}

impl ArtifactBlobCache {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ArtifactBlobCacheError> {
        let root = root.into();
        fs::create_dir_all(root.join("v1"))?;
        Ok(Self { root })
    }

    pub fn load(&self, key: &ArtifactBlobKey) -> Result<Option<Vec<u8>>, ArtifactBlobCacheError> {
        let path = self.path_for(key);
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let payload = validate_blob(&bytes, key)?;
        Ok(Some(payload.to_vec()))
    }

    /// Publishes an opaque immutable payload. Returns `true` when this caller
    /// populated the cache and `false` when a concurrent or prior writer had
    /// already published the same key.
    pub fn publish(
        &self,
        key: &ArtifactBlobKey,
        payload: &[u8],
    ) -> Result<bool, ArtifactBlobCacheError> {
        let destination = self.path_for(key);
        if destination.exists() {
            return Ok(false);
        }
        let parent = destination.parent().expect("blob path always has parent");
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".{}.tmp-{}", key.filename(), std::process::id()));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        write_blob(&mut file, key, payload)?;
        file.sync_all()?;
        drop(file);

        match fs::hard_link(&temporary, &destination) {
            Ok(()) => {
                fs::remove_file(&temporary)?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                fs::remove_file(&temporary)?;
                Ok(false)
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                Err(error.into())
            }
        }
    }

    fn path_for(&self, key: &ArtifactBlobKey) -> PathBuf {
        self.root.join("v1").join(key.filename())
    }
}

impl ArtifactBlobKey {
    fn filename(&self) -> String {
        let digest = self.digest();
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
            + ".blob"
    }
}

fn write_blob(
    file: &mut File,
    key: &ArtifactBlobKey,
    payload: &[u8],
) -> Result<(), std::io::Error> {
    file.write_all(&MAGIC)?;
    file.write_all(&1_u32.to_le_bytes())?;
    file.write_all(&key.digest())?;
    file.write_all(&(payload.len() as u64).to_le_bytes())?;
    file.write_all(payload)
}

fn validate_blob<'a>(
    bytes: &'a [u8],
    key: &ArtifactBlobKey,
) -> Result<&'a [u8], ArtifactBlobCacheError> {
    if bytes.len() < HEADER_SIZE || bytes[..8] != MAGIC || bytes[8..12] != 1_u32.to_le_bytes() {
        return Err(ArtifactBlobCacheError::InvalidHeader);
    }
    if bytes[12..44] != key.digest() {
        return Err(ArtifactBlobCacheError::InvalidHeader);
    }
    let length = u64::from_le_bytes(bytes[44..52].try_into().expect("fixed header slice"));
    let payload = &bytes[HEADER_SIZE..];
    if usize::try_from(length).ok() != Some(payload.len()) {
        return Err(ArtifactBlobCacheError::InvalidHeader);
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::{Fingerprint, Provenance, WORKER_PROTOCOL_VERSION};

    use super::*;

    fn key() -> ArtifactBlobKey {
        ArtifactBlobKey::new(
            Fingerprint::new("sha256:artifact"),
            &Provenance {
                backend: "kide-kotlin-jvm".to_owned(),
                backend_version: "0.1.0".to_owned(),
                protocol_version: WORKER_PROTOCOL_VERSION,
                analysis_options: Fingerprint::new("sha256:options"),
            },
        )
    }

    #[test]
    fn a_blob_is_shared_by_independent_project_cache_handles() {
        let directory = tempdir().expect("temporary cache directory");
        let first = ArtifactBlobCache::open(directory.path()).expect("opens first cache");
        let second = ArtifactBlobCache::open(directory.path()).expect("opens second cache");
        let key = key();

        assert!(
            first
                .publish(&key, b"opaque binary facts")
                .expect("publishes")
        );
        assert!(
            !second
                .publish(&key, b"different payload")
                .expect("reuses existing")
        );
        assert_eq!(
            second.load(&key).expect("loads"),
            Some(b"opaque binary facts".to_vec())
        );
    }

    #[test]
    fn backend_or_schema_identity_changes_the_cache_key() {
        let directory = tempdir().expect("temporary cache directory");
        let cache = ArtifactBlobCache::open(directory.path()).expect("opens cache");
        let key = key();
        let incompatible = ArtifactBlobKey::new(
            Fingerprint::new("sha256:artifact"),
            &Provenance {
                backend_version: "0.2.0".to_owned(),
                ..key_provenance()
            },
        );

        assert!(cache.publish(&key, b"v1").expect("publishes"));
        assert_eq!(cache.load(&incompatible).expect("separate key"), None);
    }

    fn key_provenance() -> Provenance {
        Provenance {
            backend: "kide-kotlin-jvm".to_owned(),
            backend_version: "0.1.0".to_owned(),
            protocol_version: WORKER_PROTOCOL_VERSION,
            analysis_options: Fingerprint::new("sha256:options"),
        }
    }
}
