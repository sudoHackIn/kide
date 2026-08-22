//! Shared, immutable storage for dependency artifact payloads.
//!
//! This cache intentionally has no SQLite dependency. A project index records
//! only a cache key; callers may load or publish blobs in parallel with index
//! transactions. Publication uses a temporary file plus an atomic hard-link,
//! so competing workers never observe a partial blob.

use std::{
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    Fingerprint, Provenance, ResolvedDependencyIdentity,
    artifact_blob_layout::ARTIFACT_BLOB_LAYOUT_VERSION,
};

const MAGIC: [u8; 8] = *b"KIDEBLB1";
const HEADER_SIZE: usize = MAGIC.len() + 4 + 32 + 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactBlobKey {
    identity: ResolvedDependencyIdentity,
}

/// Timing and outcome for promoting a worker-produced staged artifact into
/// the immutable shared cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ArtifactPromotionMetrics {
    pub promoted: bool,
    pub staged_checksum_millis: u64,
    pub cache_publish_millis: u64,
}

impl ArtifactBlobKey {
    /// Creates the complete compatibility key for a worker-produced artifact
    /// payload. Content alone is insufficient: a different resolved build
    /// context or analysis-options fingerprint must miss the shared cache.
    pub fn new(artifact: Fingerprint, context: Fingerprint, provenance: &Provenance) -> Self {
        Self {
            identity: ResolvedDependencyIdentity::unattributed(
                artifact,
                context,
                provenance.clone(),
                ARTIFACT_BLOB_LAYOUT_VERSION,
            ),
        }
    }

    pub fn from_identity(identity: ResolvedDependencyIdentity) -> Self {
        Self { identity }
    }

    pub fn identity(&self) -> &ResolvedDependencyIdentity {
        &self.identity
    }

    /// Opaque content-addressed identifier persisted by catalog rows. It does
    /// not reveal a worker-local cache path.
    pub fn cache_key(&self) -> Fingerprint {
        self.identity.cache_key()
    }

    pub fn for_descriptor(descriptor: &crate::ArtifactDescriptor) -> Self {
        Self::from_identity(descriptor.resolved_identity())
    }

    fn digest(&self) -> [u8; 32] {
        Sha256::digest(self.identity.canonical_bytes()).into()
    }
}

#[derive(Debug, Error)]
pub enum ArtifactBlobCacheError {
    #[error("artifact blob cache I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("artifact blob has an incompatible or corrupt header")]
    InvalidHeader,
    #[error("artifact blob stream ended before its declared {expected} byte payload")]
    TruncatedPayload { expected: u64 },
    #[error("staged artifact length {actual} does not match worker metadata {expected}")]
    StagedLengthMismatch { expected: u64, actual: u64 },
    #[error("staged artifact checksum does not match worker metadata")]
    StagedChecksumMismatch,
}

/// A validated payload file. Opening it reads only the fixed-size header;
/// callers can then fetch index sections by range without materialising the
/// artifact in memory.
#[derive(Debug)]
pub struct ArtifactBlob {
    file: File,
    payload_len: u64,
}

impl ArtifactBlob {
    pub fn len(&self) -> u64 {
        self.payload_len
    }

    pub fn is_empty(&self) -> bool {
        self.payload_len == 0
    }

    pub fn read_range(
        &mut self,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, ArtifactBlobCacheError> {
        let end = offset
            .checked_add(length as u64)
            .ok_or(ArtifactBlobCacheError::InvalidHeader)?;
        if end > self.payload_len {
            return Err(ArtifactBlobCacheError::InvalidHeader);
        }
        self.file
            .seek(SeekFrom::Start(HEADER_SIZE as u64 + offset))?;
        let mut bytes = vec![0; length];
        self.file.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    pub fn read_all(&mut self) -> Result<Vec<u8>, ArtifactBlobCacheError> {
        let length =
            usize::try_from(self.payload_len).map_err(|_| ArtifactBlobCacheError::InvalidHeader)?;
        self.read_range(0, length)
    }
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

    pub fn open_blob(
        &self,
        key: &ArtifactBlobKey,
    ) -> Result<Option<ArtifactBlob>, ArtifactBlobCacheError> {
        let path = self.path_for(key);
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut header = [0; HEADER_SIZE];
        file.read_exact(&mut header)?;
        let payload_len = validate_header(&header, key)?;
        if file.metadata()?.len() != HEADER_SIZE as u64 + payload_len {
            return Err(ArtifactBlobCacheError::InvalidHeader);
        }
        Ok(Some(ArtifactBlob { file, payload_len }))
    }

    pub fn load(&self, key: &ArtifactBlobKey) -> Result<Option<Vec<u8>>, ArtifactBlobCacheError> {
        self.open_blob(key)?
            .map(|mut blob| blob.read_all())
            .transpose()
    }

    /// Publishes an opaque immutable payload. Returns `true` when this caller
    /// populated the cache and `false` when a concurrent or prior writer had
    /// already published the same key.
    pub fn publish(
        &self,
        key: &ArtifactBlobKey,
        payload: &[u8],
    ) -> Result<bool, ArtifactBlobCacheError> {
        self.publish_stream(key, payload.len() as u64, Cursor::new(payload))
    }

    /// Streams a payload directly to a temporary cache file; no SQLite
    /// transaction and no full in-memory buffer are involved.
    pub fn publish_stream<R: Read>(
        &self,
        key: &ArtifactBlobKey,
        payload_len: u64,
        mut payload: R,
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
        write_blob_stream(&mut file, key, payload_len, &mut payload)?;
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

    /// Verifies then promotes a worker-staged blob without buffering its
    /// payload. Verification and publication intentionally use independent
    /// file handles: a failed checksum can therefore never publish a blob.
    pub fn promote_staged(
        &self,
        key: &ArtifactBlobKey,
        staged: impl AsRef<Path>,
        expected_length: u64,
        expected_sha256: [u8; 32],
    ) -> Result<bool, ArtifactBlobCacheError> {
        Ok(self
            .promote_staged_with_metrics(key, staged, expected_length, expected_sha256)?
            .promoted)
    }

    /// Verifies then promotes a worker-staged blob, reporting the separate
    /// costs of reading/checksumming the staging file and publishing it.
    pub fn promote_staged_with_metrics(
        &self,
        key: &ArtifactBlobKey,
        staged: impl AsRef<Path>,
        expected_length: u64,
        expected_sha256: [u8; 32],
    ) -> Result<ArtifactPromotionMetrics, ArtifactBlobCacheError> {
        let staged = staged.as_ref();
        let actual = fs::metadata(staged)?.len();
        if actual != expected_length {
            return Err(ArtifactBlobCacheError::StagedLengthMismatch {
                expected: expected_length,
                actual,
            });
        }
        let checksum_started = Instant::now();
        let mut hasher = Sha256::new();
        let mut file = File::open(staged)?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        if hasher.finalize().as_slice() != expected_sha256 {
            return Err(ArtifactBlobCacheError::StagedChecksumMismatch);
        }
        let staged_checksum_millis = checksum_started.elapsed().as_millis() as u64;
        let publish_started = Instant::now();
        let promoted = self.publish_stream(key, expected_length, File::open(staged)?)?;
        let cache_publish_millis = publish_started.elapsed().as_millis() as u64;
        Ok(ArtifactPromotionMetrics {
            promoted,
            staged_checksum_millis,
            cache_publish_millis,
        })
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

fn write_blob_stream<R: Read>(
    file: &mut File,
    key: &ArtifactBlobKey,
    payload_len: u64,
    payload: &mut R,
) -> Result<(), ArtifactBlobCacheError> {
    file.write_all(&MAGIC)?;
    file.write_all(&1_u32.to_le_bytes())?;
    file.write_all(&key.digest())?;
    file.write_all(&payload_len.to_le_bytes())?;
    let copied = std::io::copy(&mut payload.take(payload_len), file)?;
    if copied != payload_len {
        return Err(ArtifactBlobCacheError::TruncatedPayload {
            expected: payload_len,
        });
    }
    Ok(())
}

fn validate_header(bytes: &[u8], key: &ArtifactBlobKey) -> Result<u64, ArtifactBlobCacheError> {
    if bytes.len() != HEADER_SIZE || bytes[..8] != MAGIC || bytes[8..12] != 1_u32.to_le_bytes() {
        return Err(ArtifactBlobCacheError::InvalidHeader);
    }
    if bytes[12..44] != key.digest() {
        return Err(ArtifactBlobCacheError::InvalidHeader);
    }
    Ok(u64::from_le_bytes(
        bytes[44..52].try_into().expect("fixed header slice"),
    ))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::{Fingerprint, Provenance, WORKER_PROTOCOL_VERSION};

    use super::*;

    fn key() -> ArtifactBlobKey {
        ArtifactBlobKey::new(
            Fingerprint::new("sha256:artifact"),
            Fingerprint::new("sha256:component-context"),
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

        let mut blob = second.open_blob(&key).expect("opens").expect("exists");
        assert_eq!(blob.len(), 19);
        assert_eq!(blob.read_range(7, 6).expect("reads index range"), b"binary");
    }

    #[test]
    fn stream_publication_does_not_require_a_payload_buffer() {
        let directory = tempdir().expect("temporary cache directory");
        let cache = ArtifactBlobCache::open(directory.path()).expect("opens cache");
        let key = key();
        let source = Cursor::new(b"streamed payload".to_vec());

        assert!(
            cache
                .publish_stream(&key, 16, source)
                .expect("streams blob")
        );
        assert_eq!(
            cache.load(&key).expect("loads"),
            Some(b"streamed payload".to_vec())
        );
    }

    #[test]
    fn backend_or_schema_identity_changes_the_cache_key() {
        let directory = tempdir().expect("temporary cache directory");
        let cache = ArtifactBlobCache::open(directory.path()).expect("opens cache");
        let key = key();
        let incompatible = ArtifactBlobKey::new(
            Fingerprint::new("sha256:artifact"),
            Fingerprint::new("sha256:component-context"),
            &Provenance {
                backend_version: "0.2.0".to_owned(),
                ..key_provenance()
            },
        );

        assert!(cache.publish(&key, b"v1").expect("publishes"));
        assert_eq!(cache.load(&incompatible).expect("separate key"), None);
    }

    #[test]
    fn context_or_analysis_options_identity_changes_the_cache_key() {
        let directory = tempdir().expect("temporary cache directory");
        let cache = ArtifactBlobCache::open(directory.path()).expect("opens cache");
        let key = key();
        let changed_context = ArtifactBlobKey::new(
            Fingerprint::new("sha256:artifact"),
            Fingerprint::new("sha256:other-context"),
            &key_provenance(),
        );
        let changed_options = ArtifactBlobKey::new(
            Fingerprint::new("sha256:artifact"),
            Fingerprint::new("sha256:component-context"),
            &Provenance {
                analysis_options: Fingerprint::new("sha256:other-options"),
                ..key_provenance()
            },
        );

        assert!(cache.publish(&key, b"v1").expect("publishes"));
        assert_eq!(cache.load(&changed_context).expect("separate key"), None);
        assert_eq!(cache.load(&changed_options).expect("separate key"), None);
    }

    fn key_provenance() -> Provenance {
        Provenance {
            backend: "kide-kotlin-jvm".to_owned(),
            backend_version: "0.1.0".to_owned(),
            protocol_version: WORKER_PROTOCOL_VERSION,
            analysis_options: Fingerprint::new("sha256:options"),
        }
    }

    #[test]
    fn promotes_a_verified_staged_file_without_reading_it_into_memory() {
        let directory = tempdir().expect("temporary cache directory");
        let cache = ArtifactBlobCache::open(directory.path().join("cache")).expect("opens cache");
        let payload = vec![0x5a; 256 * 1024];
        let staged = directory.path().join("staged.blob");
        fs::write(&staged, &payload).expect("writes staged payload");
        let digest: [u8; 32] = Sha256::digest(&payload).into();

        assert!(
            cache
                .promote_staged(&key(), &staged, payload.len() as u64, digest)
                .expect("promotes staged blob")
        );
        assert_eq!(cache.load(&key()).expect("loads cache"), Some(payload));
    }

    #[test]
    fn rejects_bad_staged_metadata_before_publication() {
        let directory = tempdir().expect("temporary cache directory");
        let cache = ArtifactBlobCache::open(directory.path().join("cache")).expect("opens cache");
        let staged = directory.path().join("staged.blob");
        fs::write(&staged, b"payload").expect("writes staged payload");

        assert!(matches!(
            cache.promote_staged(&key(), &staged, 7, [0; 32]),
            Err(ArtifactBlobCacheError::StagedChecksumMismatch)
        ));
        assert!(cache.open_blob(&key()).expect("opens cache").is_none());
    }
}
