//! Fixed-header, section-addressable layout for JVM dependency graph blobs.

use std::collections::BTreeMap;

use prost::Message;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    ArtifactBlob, ArtifactBlobCacheError,
    artifact_proto::{self, ArtifactBlobSectionKind},
};

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
    #[error("artifact blob cache read failed: {0}")]
    Cache(#[from] ArtifactBlobCacheError),
}

/// Section table read from a cache file without loading its graph payload.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactBlobSections {
    toc: artifact_proto::ArtifactBlobToc,
}

impl ArtifactBlobSections {
    pub fn open(blob: &mut ArtifactBlob) -> Result<Self, ArtifactBlobLayoutError> {
        let header = blob.read_range(0, HEADER_SIZE)?;
        let toc_location = toc_location(&header)?;
        let toc_bytes = blob.read_range(toc_location.offset, toc_location.length)?;
        let toc = parse_toc(toc_location, blob.len(), &toc_bytes)?;
        Ok(Self { toc })
    }

    pub fn read_section(
        &self,
        blob: &mut ArtifactBlob,
        kind: ArtifactBlobSectionKind,
    ) -> Result<Vec<u8>, ArtifactBlobLayoutError> {
        let section = self
            .toc
            .sections
            .iter()
            .find(|section| section.kind == kind as i32)
            .ok_or(ArtifactBlobLayoutError::MissingSection(kind))?;
        let length =
            usize::try_from(section.length).map_err(|_| ArtifactBlobLayoutError::InvalidHeader)?;
        let bytes = blob.read_range(section.offset, length)?;
        if section.sha256 != Sha256::digest(&bytes).as_slice() {
            return Err(ArtifactBlobLayoutError::ChecksumMismatch);
        }
        Ok(bytes)
    }

    pub fn symbol_postings(
        &self,
        blob: &mut ArtifactBlob,
    ) -> Result<artifact_proto::ArtifactSymbolPostings, ArtifactBlobLayoutError> {
        Ok(artifact_proto::ArtifactSymbolPostings::decode(
            self.read_section(blob, ArtifactBlobSectionKind::SymbolPostings)?
                .as_slice(),
        )?)
    }
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
        let postings = artifact_proto::ArtifactSymbolPostings {
            entries: artifact
                .snapshots
                .iter()
                .enumerate()
                .flat_map(|(source_unit_index, snapshot)| {
                    snapshot.symbols.iter().map(move |symbol| {
                        artifact_proto::ArtifactSymbolPosting {
                            source_unit_index: source_unit_index as u32,
                            id: symbol.id.clone(),
                            name: symbol.name.clone(),
                            qualified_name: symbol.qualified_name.clone(),
                            declaration: symbol.declaration.clone(),
                            name_range: symbol.name_range.clone(),
                            kind: symbol.kind.clone(),
                        }
                    })
                })
                .collect(),
        };
        let sections: BTreeMap<ArtifactBlobSectionKind, Vec<u8>> = BTreeMap::from([
            (
                ArtifactBlobSectionKind::SymbolDictionary,
                dictionary.encode_to_vec(),
            ),
            (
                ArtifactBlobSectionKind::SymbolPostings,
                postings.encode_to_vec(),
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
        let header = bytes
            .get(..HEADER_SIZE)
            .ok_or(ArtifactBlobLayoutError::InvalidHeader)?;
        let toc_location = toc_location(header)?;
        let toc_end = toc_location
            .offset
            .checked_add(toc_location.length as u64)
            .ok_or(ArtifactBlobLayoutError::InvalidHeader)?;
        let toc_bytes = bytes
            .get(toc_location.offset as usize..toc_end as usize)
            .ok_or(ArtifactBlobLayoutError::InvalidHeader)?;
        let toc = parse_toc(toc_location, bytes.len() as u64, toc_bytes)?;
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

    /// Reads only the compact declaration postings section. Callers use this
    /// for name navigation without touching the complete graph section.
    pub fn symbol_postings(
        &self,
    ) -> Result<artifact_proto::ArtifactSymbolPostings, ArtifactBlobLayoutError> {
        Ok(artifact_proto::ArtifactSymbolPostings::decode(
            self.section(ArtifactBlobSectionKind::SymbolPostings)?,
        )?)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Debug, Clone, Copy)]
struct TocLocation {
    offset: u64,
    length: usize,
}

fn toc_location(header: &[u8]) -> Result<TocLocation, ArtifactBlobLayoutError> {
    if header.len() != HEADER_SIZE || header[..8] != MAGIC {
        return Err(ArtifactBlobLayoutError::InvalidHeader);
    }
    let version = u32::from_le_bytes(header[8..12].try_into().expect("fixed slice"));
    if version != ARTIFACT_BLOB_LAYOUT_VERSION {
        return Err(ArtifactBlobLayoutError::UnsupportedVersion(version));
    }
    let toc_offset = u64::from_le_bytes(header[12..20].try_into().expect("fixed slice"));
    if toc_offset != HEADER_SIZE as u64 {
        return Err(ArtifactBlobLayoutError::InvalidHeader);
    }
    let toc_length = u64::from_le_bytes(header[20..28].try_into().expect("fixed slice"));
    usize::try_from(toc_length)
        .map(|length| TocLocation {
            offset: toc_offset,
            length,
        })
        .map_err(|_| ArtifactBlobLayoutError::InvalidHeader)
}

fn parse_toc(
    toc_location: TocLocation,
    payload_len: u64,
    toc_bytes: &[u8],
) -> Result<artifact_proto::ArtifactBlobToc, ArtifactBlobLayoutError> {
    let toc_end = toc_location
        .offset
        .checked_add(toc_location.length as u64)
        .ok_or(ArtifactBlobLayoutError::InvalidHeader)?;
    if toc_bytes.len() != toc_location.length || toc_end > payload_len {
        return Err(ArtifactBlobLayoutError::InvalidHeader);
    }
    let toc = artifact_proto::ArtifactBlobToc::decode(toc_bytes)?;
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
        if section.offset < toc_end || end > payload_len || section.sha256.len() != 32 {
            return Err(ArtifactBlobLayoutError::InvalidHeader);
        }
    }
    Ok(toc)
}
