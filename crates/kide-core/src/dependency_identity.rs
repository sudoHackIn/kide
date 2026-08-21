//! Durable compatibility identity for one resolved dependency artifact.
//!
//! The identity is deliberately richer than a package coordinate: local files,
//! JDK modules, and artifacts from non-registry repositories have no reliable
//! coordinate, but can still be safely reused by content and analysis context.

use sha2::{Digest, Sha256};

use crate::{Fingerprint, Provenance, CANONICAL_SCHEMA_VERSION};

/// Version of the serialized dependency identity contract.
pub const DEPENDENCY_IDENTITY_VERSION: u32 = 1;

/// Version of the immutable artifact payload layout expected by the consumer.
pub const ARTIFACT_BLOB_FORMAT_VERSION: u32 = 1;

/// Immutable compatibility identity for a resolved dependency analysis blob.
///
/// `canonical_coordinate` and `resolved_version` are intentionally optional:
/// content-addressed local artifacts and platform modules must not invent a
/// registry identity merely to participate in the cache.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResolvedDependencyIdentity {
    pub identity_version: u32,
    pub ecosystem: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_coordinate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_version: Option<String>,
    pub content: Fingerprint,
    pub context: Fingerprint,
    pub provenance: Provenance,
    pub canonical_schema_version: u32,
    pub blob_format_version: u32,
}

impl ResolvedDependencyIdentity {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ecosystem: impl Into<String>,
        canonical_coordinate: Option<String>,
        resolved_version: Option<String>,
        content: Fingerprint,
        context: Fingerprint,
        provenance: Provenance,
        blob_format_version: u32,
    ) -> Self {
        Self {
            identity_version: DEPENDENCY_IDENTITY_VERSION,
            ecosystem: ecosystem.into(),
            canonical_coordinate,
            resolved_version,
            content,
            context,
            provenance,
            canonical_schema_version: CANONICAL_SCHEMA_VERSION,
            blob_format_version,
        }
    }

    /// Temporary compatibility identity for existing workers that do not yet
    /// report a registry coordinate. It remains safe because content, context,
    /// provenance, and both schema versions are still exact-match inputs.
    pub fn unattributed(
        content: Fingerprint,
        context: Fingerprint,
        provenance: Provenance,
        blob_format_version: u32,
    ) -> Self {
        Self::new(
            "unknown",
            None,
            None,
            content,
            context,
            provenance,
            blob_format_version,
        )
    }

    /// Stable JSON bytes used as the input to the opaque cache key. This
    /// struct has no maps, so serde emits fields in declaration order.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("dependency identity always serializes")
    }

    pub fn cache_key(&self) -> Fingerprint {
        Fingerprint::new(format!("sha256:{:x}", Sha256::digest(self.canonical_bytes())))
    }
}

#[cfg(test)]
mod tests {
    use crate::{Fingerprint, Provenance, WORKER_PROTOCOL_VERSION};

    use super::*;

    fn provenance() -> Provenance {
        Provenance {
            backend: "kide-kotlin-jvm".into(),
            backend_version: "0.1.0".into(),
            protocol_version: WORKER_PROTOCOL_VERSION,
            analysis_options: Fingerprint::new("sha256:options"),
        }
    }

    #[test]
    fn identity_serialization_is_versioned_and_preserves_missing_coordinate() {
        let identity = ResolvedDependencyIdentity::unattributed(
            Fingerprint::new("sha256:content"),
            Fingerprint::new("sha256:context"),
            provenance(),
            ARTIFACT_BLOB_FORMAT_VERSION,
        );

        assert_eq!(
            String::from_utf8(identity.canonical_bytes()).expect("utf8 json"),
            "{\"identity_version\":1,\"ecosystem\":\"unknown\",\"content\":\"sha256:content\",\"context\":\"sha256:context\",\"provenance\":{\"backend\":\"kide-kotlin-jvm\",\"backend_version\":\"0.1.0\",\"protocol_version\":3,\"analysis_options\":\"sha256:options\"},\"canonical_schema_version\":1,\"blob_format_version\":1}"
        );
        assert_ne!(
            identity.cache_key(),
            ResolvedDependencyIdentity::new(
                "maven",
                Some("org.example:library".into()),
                Some("1.0.0".into()),
                Fingerprint::new("sha256:content"),
                Fingerprint::new("sha256:context"),
                provenance(),
                ARTIFACT_BLOB_FORMAT_VERSION,
            )
            .cache_key()
        );
    }
}
