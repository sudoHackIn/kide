//! Fixed-header, section-addressable layout for JVM dependency graph blobs.

use std::io::{Read, Write};

use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use prost::Message;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    artifact_proto::{self, ArtifactBlobSectionKind},
    ArtifactBlob, ArtifactBlobCacheError,
};

pub const ARTIFACT_BLOB_LAYOUT_VERSION: u32 = 1;
/// Upper bound on declarations decoded for one selected ordinal lookup.
pub const SYMBOL_DETAIL_BLOCK_ENTRY_CAPACITY: usize = 256;
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
    #[error("artifact blob uses unsupported section compression {0}")]
    UnsupportedCompression(i32),
    #[error("artifact blob gzip operation failed: {0}")]
    Gzip(#[source] std::io::Error),
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
        decompress_section(&bytes, section.compression)
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

    pub fn hierarchy_postings(
        &self,
        blob: &mut ArtifactBlob,
    ) -> Result<artifact_proto::ArtifactHierarchyPostings, ArtifactBlobLayoutError> {
        Ok(artifact_proto::ArtifactHierarchyPostings::decode(
            self.read_section(blob, ArtifactBlobSectionKind::HierarchyPostings)?
                .as_slice(),
        )?)
    }

    pub fn qualified_symbol_directory(
        &self,
        blob: &mut ArtifactBlob,
    ) -> Result<artifact_proto::ArtifactQualifiedSymbolDirectory, ArtifactBlobLayoutError> {
        Ok(artifact_proto::ArtifactQualifiedSymbolDirectory::decode(
            self.read_section(blob, ArtifactBlobSectionKind::QualifiedSymbolDirectory)?
                .as_slice(),
        )?)
    }

    pub fn graph_facts(
        &self,
        blob: &mut ArtifactBlob,
    ) -> Result<artifact_proto::GraphArtifact, ArtifactBlobLayoutError> {
        Ok(artifact_proto::GraphArtifact::decode(
            self.read_section(blob, ArtifactBlobSectionKind::GraphFacts)?
                .as_slice(),
        )?)
    }

    /// Reads the one detail block whose TOC ordinal interval contains `ordinal`.
    pub fn symbol_detail_block(
        &self,
        blob: &mut ArtifactBlob,
        ordinal: u32,
    ) -> Result<artifact_proto::ArtifactSymbolDetailBlock, ArtifactBlobLayoutError> {
        let section = self.detail_section(ordinal)?;
        let length =
            usize::try_from(section.length).map_err(|_| ArtifactBlobLayoutError::InvalidHeader)?;
        let bytes = blob.read_range(section.offset, length)?;
        if section.sha256 != Sha256::digest(&bytes).as_slice() {
            return Err(ArtifactBlobLayoutError::ChecksumMismatch);
        }
        Ok(artifact_proto::ArtifactSymbolDetailBlock::decode(
            decompress_section(&bytes, section.compression)?.as_slice(),
        )?)
    }

    fn detail_section(
        &self,
        ordinal: u32,
    ) -> Result<&artifact_proto::ArtifactBlobSection, ArtifactBlobLayoutError> {
        self.toc
            .sections
            .iter()
            .find(|section| {
                section.kind == ArtifactBlobSectionKind::SymbolDetailBlock as i32
                    && section.symbol_ordinal_start <= ordinal
                    && ordinal < section.symbol_ordinal_end
            })
            .ok_or(ArtifactBlobLayoutError::MissingSection(
                ArtifactBlobSectionKind::SymbolDetailBlock,
            ))
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
                            symbol: Some(symbol.clone()),
                        }
                    })
                })
                .collect(),
        };
        let hierarchy_postings = artifact_proto::ArtifactHierarchyPostings {
            entries: artifact
                .snapshots
                .iter()
                .flat_map(|snapshot| snapshot.hierarchy.iter())
                .map(|edge| artifact_proto::ArtifactHierarchyPosting {
                    subtype_symbol_id: edge.subtype_symbol_id.clone(),
                    supertype_symbol_id: edge.supertype_symbol_id.clone(),
                    precision: edge.precision.clone(),
                })
                .collect(),
        };
        let mut directory_entries = artifact
            .snapshots
            .iter()
            .flat_map(|snapshot| snapshot.symbols.iter())
            .enumerate()
            .filter_map(|(ordinal, symbol)| {
                symbol.qualified_name.as_ref().map(|qualified_name| {
                    artifact_proto::ArtifactQualifiedSymbolEntry {
                        qualified_name: qualified_name.clone(),
                        symbol_ordinal: ordinal as u32,
                    }
                })
            })
            .collect::<Vec<_>>();
        directory_entries.sort_by(|left, right| {
            (&left.qualified_name, left.symbol_ordinal)
                .cmp(&(&right.qualified_name, right.symbol_ordinal))
        });
        let qualified_symbol_directory = artifact_proto::ArtifactQualifiedSymbolDirectory {
            entries: directory_entries,
        };
        let mut sections: Vec<(ArtifactBlobSectionKind, Vec<u8>, i32, u32, u32)> = vec![
            (
                ArtifactBlobSectionKind::SymbolDictionary,
                dictionary.encode_to_vec(),
                0,
                0,
                0,
            ),
            (
                ArtifactBlobSectionKind::SymbolPostings,
                postings.encode_to_vec(),
                0,
                0,
                0,
            ),
            (
                ArtifactBlobSectionKind::HierarchyPostings,
                hierarchy_postings.encode_to_vec(),
                0,
                0,
                0,
            ),
            (
                ArtifactBlobSectionKind::QualifiedSymbolDirectory,
                qualified_symbol_directory.encode_to_vec(),
                0,
                0,
                0,
            ),
            (
                ArtifactBlobSectionKind::GraphFacts,
                gzip(&artifact.encode_to_vec()),
                1,
                0,
                0,
            ),
        ];
        let mut first_ordinal = 0;
        for snapshot in &artifact.snapshots {
            let count = snapshot.symbols.len() as u32;
            for (chunk_index, entries) in snapshot
                .symbols
                .chunks(SYMBOL_DETAIL_BLOCK_ENTRY_CAPACITY)
                .enumerate()
            {
                let chunk_start =
                    first_ordinal + (chunk_index * SYMBOL_DETAIL_BLOCK_ENTRY_CAPACITY) as u32;
                let defaults = snapshot_defaults(snapshot, entries);
                let block = artifact_proto::ArtifactSymbolDetailBlock {
                    first_symbol_ordinal: chunk_start,
                    defaults: Some(defaults.clone()),
                    entries: entries
                        .iter()
                        .map(|symbol| symbol_detail(symbol, &defaults))
                        .collect(),
                };
                sections.push((
                    ArtifactBlobSectionKind::SymbolDetailBlock,
                    block.encode_to_vec(),
                    0,
                    chunk_start,
                    chunk_start + entries.len() as u32,
                ));
            }
            first_ordinal += count;
        }
        let mut toc = artifact_proto::ArtifactBlobToc {
            layout_version: ARTIFACT_BLOB_LAYOUT_VERSION,
            sections: Vec::new(),
        };
        loop {
            let mut offset = (HEADER_SIZE + toc.encoded_len()) as u64;
            let next = sections
                .iter()
                .map(|(kind, bytes, compression, ordinal_start, ordinal_end)| {
                    let section = artifact_proto::ArtifactBlobSection {
                        kind: *kind as i32,
                        offset,
                        length: bytes.len() as u64,
                        sha256: Sha256::digest(bytes).to_vec(),
                        compression: *compression,
                        symbol_ordinal_start: *ordinal_start,
                        symbol_ordinal_end: *ordinal_end,
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
            HEADER_SIZE
                + toc_bytes.len()
                + sections
                    .iter()
                    .map(|(_, bytes, _, _, _)| bytes.len())
                    .sum::<usize>(),
        );
        bytes.extend(MAGIC);
        bytes.extend(ARTIFACT_BLOB_LAYOUT_VERSION.to_le_bytes());
        bytes.extend((HEADER_SIZE as u64).to_le_bytes());
        bytes.extend((toc_bytes.len() as u64).to_le_bytes());
        bytes.extend(toc_bytes);
        bytes.extend(sections.iter().flat_map(|(_, bytes, _, _, _)| bytes));
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

    pub fn hierarchy_postings(
        &self,
    ) -> Result<artifact_proto::ArtifactHierarchyPostings, ArtifactBlobLayoutError> {
        Ok(artifact_proto::ArtifactHierarchyPostings::decode(
            self.section(ArtifactBlobSectionKind::HierarchyPostings)?,
        )?)
    }

    pub fn qualified_symbol_directory(
        &self,
    ) -> Result<artifact_proto::ArtifactQualifiedSymbolDirectory, ArtifactBlobLayoutError> {
        Ok(artifact_proto::ArtifactQualifiedSymbolDirectory::decode(
            self.section(ArtifactBlobSectionKind::QualifiedSymbolDirectory)?,
        )?)
    }

    pub fn graph_facts(&self) -> Result<artifact_proto::GraphArtifact, ArtifactBlobLayoutError> {
        let section = self.section_descriptor(ArtifactBlobSectionKind::GraphFacts)?;
        Ok(artifact_proto::GraphArtifact::decode(
            decompress_section(
                self.section(ArtifactBlobSectionKind::GraphFacts)?,
                section.compression,
            )?
            .as_slice(),
        )?)
    }

    pub fn symbol_detail_block(
        &self,
        ordinal: u32,
    ) -> Result<artifact_proto::ArtifactSymbolDetailBlock, ArtifactBlobLayoutError> {
        let section = self.detail_section(ordinal)?;
        Ok(artifact_proto::ArtifactSymbolDetailBlock::decode(
            decompress_section(self.section_by_descriptor(section)?, section.compression)?
                .as_slice(),
        )?)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn section_descriptor(
        &self,
        kind: ArtifactBlobSectionKind,
    ) -> Result<&artifact_proto::ArtifactBlobSection, ArtifactBlobLayoutError> {
        self.toc
            .sections
            .iter()
            .find(|section| section.kind == kind as i32)
            .ok_or(ArtifactBlobLayoutError::MissingSection(kind))
    }

    fn detail_section(
        &self,
        ordinal: u32,
    ) -> Result<&artifact_proto::ArtifactBlobSection, ArtifactBlobLayoutError> {
        self.toc
            .sections
            .iter()
            .find(|section| {
                section.kind == ArtifactBlobSectionKind::SymbolDetailBlock as i32
                    && section.symbol_ordinal_start <= ordinal
                    && ordinal < section.symbol_ordinal_end
            })
            .ok_or(ArtifactBlobLayoutError::MissingSection(
                ArtifactBlobSectionKind::SymbolDetailBlock,
            ))
    }

    fn section_by_descriptor(
        &self,
        section: &artifact_proto::ArtifactBlobSection,
    ) -> Result<&[u8], ArtifactBlobLayoutError> {
        let bytes =
            &self.bytes[section.offset as usize..(section.offset + section.length) as usize];
        if section.sha256 != Sha256::digest(bytes).as_slice() {
            return Err(ArtifactBlobLayoutError::ChecksumMismatch);
        }
        Ok(bytes)
    }
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(bytes).expect("Vec writes cannot fail");
    encoder.finish().expect("Vec writes cannot fail")
}

fn snapshot_defaults(
    snapshot: &artifact_proto::GraphSnapshot,
    symbols: &[artifact_proto::ArtifactSymbol],
) -> artifact_proto::ArtifactSymbolSnapshotDefaults {
    artifact_proto::ArtifactSymbolSnapshotDefaults {
        source_unit: snapshot.source_unit.clone(),
        provenances: snapshot.provenances.clone(),
        language: common_symbol_field(symbols, |symbol| &symbol.language),
        freshness: common_symbol_field(symbols, |symbol| &symbol.freshness),
        completeness: common_symbol_field(symbols, |symbol| &symbol.completeness),
        component_id: common_symbol_field(symbols, |symbol| &symbol.component_id),
        provenance_index: common_symbol_option(symbols, |symbol| symbol.provenance_index),
    }
}

fn common_symbol_field<'a>(
    symbols: &'a [artifact_proto::ArtifactSymbol],
    field: impl Fn(&'a artifact_proto::ArtifactSymbol) -> &'a String,
) -> String {
    let Some(first) = symbols.first() else {
        return String::new();
    };
    let value = field(first);
    if symbols.iter().all(|symbol| field(symbol) == value) {
        value.clone()
    } else {
        String::new()
    }
}

fn common_symbol_option(
    symbols: &[artifact_proto::ArtifactSymbol],
    field: impl Fn(&artifact_proto::ArtifactSymbol) -> Option<u32>,
) -> Option<u32> {
    let value = symbols.first().and_then(&field);
    if symbols.iter().all(|symbol| field(symbol) == value) {
        value
    } else {
        None
    }
}

fn symbol_detail(
    symbol: &artifact_proto::ArtifactSymbol,
    defaults: &artifact_proto::ArtifactSymbolSnapshotDefaults,
) -> artifact_proto::ArtifactSymbolDetail {
    artifact_proto::ArtifactSymbolDetail {
        id: symbol.id.clone(),
        backend_key: symbol.backend_key.clone(),
        backend_schema_version: symbol.backend_schema_version,
        kind: symbol.kind.clone(),
        name: symbol.name.clone(),
        qualified_name: symbol.qualified_name.clone(),
        signature: symbol.signature.clone(),
        declaration: symbol.declaration.clone(),
        name_range: symbol.name_range.clone(),
        owner_id: symbol.owner_id.clone(),
        modifiers: symbol.modifiers.clone(),
        applied_symbol_ids: symbol.applied_symbol_ids.clone(),
        language: (symbol.language != defaults.language).then(|| symbol.language.clone()),
        freshness: (symbol.freshness != defaults.freshness).then(|| symbol.freshness.clone()),
        completeness: (symbol.completeness != defaults.completeness)
            .then(|| symbol.completeness.clone()),
        component_id: (symbol.component_id != defaults.component_id)
            .then(|| symbol.component_id.clone()),
        provenance_index: (symbol.provenance_index != defaults.provenance_index)
            .then_some(symbol.provenance_index)
            .flatten(),
    }
}

fn decompress_section(bytes: &[u8], compression: i32) -> Result<Vec<u8>, ArtifactBlobLayoutError> {
    match compression {
        0 => Ok(bytes.to_vec()),
        1 => {
            let mut decoded = Vec::new();
            GzDecoder::new(bytes)
                .read_to_end(&mut decoded)
                .map_err(ArtifactBlobLayoutError::Gzip)?;
            Ok(decoded)
        }
        value => Err(ArtifactBlobLayoutError::UnsupportedCompression(value)),
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
