//! Fixed-header, section-addressable layout for JVM dependency graph blobs.

use std::collections::BTreeMap;

use prost::Message;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::artifact_proto::{self, ArtifactBlobSectionKind};

pub const ARTIFACT_BLOB_LAYOUT_VERSION: u32 = 1;
const MAGIC: [u8; 8] = *b"KIDEJVM1";
const HEADER_SIZE: usize = 8 + 4 + 8 + 8;

#[derive(Debug, Error)]
pub enum ArtifactBlobLayoutError {
    #[error("artifact blob has an invalid or incompatible header")]
    InvalidHeader,
    #[error("artifact blob uses unsupported layout version {0}")]
    UnsupportedVersion(u32),
    #[error("artifact blob TOC cannot be decoded: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("artifact blob is missing {0:?} section")]
    MissingSection(ArtifactBlobSectionKind),
    #[error("artifact blob section checksum does not match")]
    ChecksumMismatch,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactBlobLayout {
    bytes: Vec<u8>,
    toc: artifact_proto::ArtifactBlobToc,
}

impl ArtifactBlobLayout {
    pub fn encode(artifact: &artifact_proto::GraphArtifact) -> Self {
        let dictionary = artifact_proto::ArtifactSymbolDictionary {
            entries: artifact
                .snapshots
                .iter()
                .flat_map(|snapshot| snapshot.symbols.iter())
                .map(|symbol| artifact_proto::ArtifactSymbolDictionaryEntry {
                    id: symbol.id.clone(),
                    name: symbol.name.clone(),
                })
                .collect(),
        };
        let sections: BTreeMap<ArtifactBlobSectionKind, Vec<u8>> = BTreeMap::from([
            (
                ArtifactBlobSectionKind::SymbolDictionary,
                dictionary.encode_to_vec(),
            ),
            (
                ArtifactBlobSectionKind::GraphFacts,
                artifact.encode_to_vec(),
            ),
        ]);
        let mut toc = artifact_proto::ArtifactBlobToc {
            layout_version: ARTIFACT_BLOB_LAYOUT_VERSION,
            sections: Vec::new(),
        };
        loop {
            let mut offset = (HEADER_SIZE + toc.encoded_len()) as u64;
            let next = sections
                .iter()
                .map(|(kind, bytes)| {
                    let section = artifact_proto::ArtifactBlobSection {
                        kind: *kind as i32,
                        offset,
                        length: bytes.len() as u64,
                        sha256: Sha256::digest(bytes).to_vec(),
                    };
                    offset += bytes.len() as u64;
                    section
                })
                .collect::<Vec<_>>();
            if next == toc.sections {
                break;
            }
            toc.sections = next;
        }
        let toc_bytes = toc.encode_to_vec();
        let mut bytes = Vec::with_capacity(
            HEADER_SIZE + toc_bytes.len() + sections.values().map(Vec::len).sum::<usize>(),
        );
        bytes.extend(MAGIC);
        bytes.extend(ARTIFACT_BLOB_LAYOUT_VERSION.to_le_bytes());
        bytes.extend((HEADER_SIZE as u64).to_le_bytes());
        bytes.extend((toc_bytes.len() as u64).to_le_bytes());
        bytes.extend(toc_bytes);
        bytes.extend(sections.values().flatten());
        Self { bytes, toc }
    }

    pub fn validate(bytes: Vec<u8>) -> Result<Self, ArtifactBlobLayoutError> {
        if bytes.len() < HEADER_SIZE || bytes[..8] != MAGIC {
            return Err(ArtifactBlobLayoutError::InvalidHeader);
        }
        let version = u32::from_le_bytes(bytes[8..12].try_into().expect("fixed slice"));
        if version != ARTIFACT_BLOB_LAYOUT_VERSION {
            return Err(ArtifactBlobLayoutError::UnsupportedVersion(version));
        }
        let toc_offset = u64::from_le_bytes(bytes[12..20].try_into().expect("fixed slice"));
        let toc_length = u64::from_le_bytes(bytes[20..28].try_into().expect("fixed slice"));
        if toc_offset != HEADER_SIZE as u64 {
            return Err(ArtifactBlobLayoutError::InvalidHeader);
        }
        let toc_end = toc_offset
            .checked_add(toc_length)
            .ok_or(ArtifactBlobLayoutError::InvalidHeader)?;
        if toc_end > bytes.len() as u64 {
            return Err(ArtifactBlobLayoutError::InvalidHeader);
        }
        let toc =
            artifact_proto::ArtifactBlobToc::decode(&bytes[toc_offset as usize..toc_end as usize])?;
        if toc.layout_version != ARTIFACT_BLOB_LAYOUT_VERSION {
            return Err(ArtifactBlobLayoutError::UnsupportedVersion(
                toc.layout_version,
            ));
        }
        for section in &toc.sections {
            let end = section
                .offset
                .checked_add(section.length)
                .ok_or(ArtifactBlobLayoutError::InvalidHeader)?;
            if section.offset < toc_end || end > bytes.len() as u64 || section.sha256.len() != 32 {
                return Err(ArtifactBlobLayoutError::InvalidHeader);
            }
        }
        Ok(Self { bytes, toc })
    }

    pub fn section(&self, kind: ArtifactBlobSectionKind) -> Result<&[u8], ArtifactBlobLayoutError> {
        let section = self
            .toc
            .sections
            .iter()
            .find(|section| section.kind == kind as i32)
            .ok_or(ArtifactBlobLayoutError::MissingSection(kind))?;
        let bytes =
            &self.bytes[section.offset as usize..(section.offset + section.length) as usize];
        if section.sha256 != Sha256::digest(bytes).as_slice() {
            return Err(ArtifactBlobLayoutError::ChecksumMismatch);
        }
        Ok(bytes)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
