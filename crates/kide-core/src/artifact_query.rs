//! On-demand semantic queries over one immutable dependency artifact blob.

use std::collections::{BTreeMap, HashMap, HashSet};

use thiserror::Error;

use crate::{
    ArtifactBlobCache, ArtifactBlobCacheError, ArtifactBlobKey, ArtifactDescriptor, SymbolId,
    SymbolRecord,
    artifact_blob_layout::{ArtifactBlobLayout, ArtifactBlobLayoutError, ArtifactBlobSections},
    artifact_proto,
    artifact_proto_adapter::{self, ArtifactProtoError},
};

#[derive(Debug, Error)]
pub enum ArtifactQueryError {
    #[error("artifact blob cache failed: {0}")]
    Cache(#[from] ArtifactBlobCacheError),
    #[error("artifact blob layout failed: {0}")]
    Layout(#[from] ArtifactBlobLayoutError),
    #[error("artifact graph failed to decode: {0}")]
    Graph(#[from] ArtifactProtoError),
    #[error("artifact graph protobuf failed to decode: {0}")]
    Decode(#[from] prost::DecodeError),
}

/// Resolves one exact qualified name through the compact directory and only
/// reads the detail blocks containing matching artifact-local ordinals.
pub fn symbols_with_qualified_name(
    cache: &ArtifactBlobCache,
    artifact: &ArtifactDescriptor,
    qualified_name: &str,
) -> Result<Vec<SymbolRecord>, ArtifactQueryError> {
    let key = ArtifactBlobKey::for_descriptor(artifact);
    let Some(mut blob) = cache.open_blob(&key)? else {
        return Ok(Vec::new());
    };
    let sections = ArtifactBlobSections::open(&mut blob)?;
    match sections.qualified_symbol_directory(&mut blob) {
        Ok(directory) => {
            let ordinals = directory
                .entries
                .into_iter()
                .filter(|entry| entry.qualified_name == qualified_name)
                .map(|entry| entry.symbol_ordinal);
            let mut symbols = Vec::new();
            for ordinal in ordinals {
                let block = sections.symbol_detail_block(&mut blob, ordinal)?;
                symbols.push(artifact_proto_adapter::decode_symbol_detail(
                    block, ordinal,
                )?);
            }
            Ok(symbols)
        }
        Err(error) => Err(error.into()),
    }
}

/// Returns direct implementations contained in `artifact` for `supertype`.
/// This is an explicit semantic traversal, so it reads GraphFacts only after
/// catalog routing selected the artifact.
pub fn direct_implementations(
    cache: &ArtifactBlobCache,
    artifact: &ArtifactDescriptor,
    supertype: &SymbolId,
) -> Result<Vec<SymbolRecord>, ArtifactQueryError> {
    let key = ArtifactBlobKey::for_descriptor(artifact);
    let Some(mut blob) = cache.open_blob(&key)? else {
        return Ok(Vec::new());
    };
    let sections = ArtifactBlobSections::open(&mut blob)?;
    match sections.hierarchy_postings(&mut blob) {
        Ok(hierarchy) => {
            let subtype_ids = hierarchy
                .entries
                .into_iter()
                .filter(|edge| edge.supertype_symbol_id == supertype.as_str())
                .map(|edge| edge.subtype_symbol_id)
                .collect::<HashSet<_>>();
            if subtype_ids.is_empty() {
                return Ok(Vec::new());
            }
            let ordinals = sections
                .symbol_dictionary(&mut blob)?
                .entries
                .into_iter()
                .enumerate()
                .filter_map(|(ordinal, entry)| {
                    subtype_ids.contains(&entry.id).then_some(ordinal as u32)
                });
            let mut by_block = BTreeMap::<u32, Vec<u32>>::new();
            for ordinal in ordinals {
                by_block
                    .entry(sections.symbol_detail_block_start(ordinal)?)
                    .or_default()
                    .push(ordinal);
            }
            let mut symbols = Vec::new();
            for (_, ordinals) in by_block {
                let block = sections.symbol_detail_block(&mut blob, ordinals[0])?;
                for ordinal in ordinals {
                    symbols.push(artifact_proto_adapter::decode_symbol_detail(
                        block.clone(),
                        ordinal,
                    )?);
                }
            }
            return Ok(symbols);
        }
        Err(ArtifactBlobLayoutError::MissingSection(
            artifact_proto::ArtifactBlobSectionKind::HierarchyPostings,
        )) => {}
        Err(error) => return Err(error.into()),
    }

    // Layout v1 blobs created before hierarchy postings retain the previous
    // bounded fallback: decode GraphFacts only for this selected artifact.
    let payload = blob.read_all()?;
    let layout = ArtifactBlobLayout::validate(payload)?;
    let graph = layout.graph_facts()?;
    let snapshots = artifact_proto_adapter::decode_graph_artifact(graph)?;
    let symbols = snapshots
        .iter()
        .flat_map(|snapshot| snapshot.symbols.iter())
        .map(|symbol| (symbol.id.clone(), symbol.clone()))
        .collect::<HashMap<_, _>>();
    Ok(snapshots
        .into_iter()
        .flat_map(|snapshot| snapshot.hierarchy)
        .filter(|edge| edge.supertype == *supertype)
        .filter_map(|edge| symbols.get(&edge.subtype).cloned())
        .collect())
}
