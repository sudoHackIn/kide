//! Deterministic resolution of user locations to persisted semantic symbols.

use thiserror::Error;

use crate::{
    IndexStore, IndexStoreError, Location, SourceUnitId, SymbolId, SymbolKind, TextPosition,
    WorkspacePath,
};

/// The outcome of resolving one location before a navigation command chooses
/// its relation (definition, references, callers, and so on).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocationResolution {
    Symbol(SymbolId),
    NoResult,
    Ambiguous { candidates: Vec<SymbolId> },
}

#[derive(Debug, Error)]
pub enum LocationResolutionError {
    #[error("index lookup failed: {0}")]
    Store(#[from] IndexStoreError),
    #[error("location line and column are one-based and must be positive")]
    InvalidCoordinate,
    #[error("location is outside the indexed source text")]
    OutOfRange,
    #[error("workspace path is not indexed: {0}")]
    UnknownPath(String),
    #[error("workspace path is indexed by multiple source units: {0}")]
    AmbiguousPath(String),
}

/// Resolves a location in the supplied source snapshot. The frontend owns
/// reading source text, while Core owns all byte/Unicode conversion rules.
pub fn resolve_location(
    store: &IndexStore,
    location: &Location,
    source_text: &str,
) -> Result<LocationResolution, LocationResolutionError> {
    let source_unit = unique_source_unit(store, &location.path)?;
    let offset = byte_offset(source_text, location.position)?;

    let mut occurrence_targets = store
        .occurrences_at(&source_unit, offset)?
        .into_iter()
        .filter_map(|occurrence| occurrence.target)
        .collect::<Vec<_>>();
    occurrence_targets.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    occurrence_targets.dedup_by(|left, right| left.as_str() == right.as_str());
    if let Some(resolution) = return_resolution(occurrence_targets) {
        return Ok(resolution);
    }
    let declarations = store
        .symbols_for_source(&source_unit)?
        .into_iter()
        .filter(|symbol| {
            symbol.name_range.bytes.start <= offset && offset < symbol.name_range.bytes.end
        })
        .collect::<Vec<_>>();
    Ok(resolve_declaration_candidates(declarations).unwrap_or(LocationResolution::NoResult))
}

fn unique_source_unit(
    store: &IndexStore,
    path: &WorkspacePath,
) -> Result<SourceUnitId, LocationResolutionError> {
    let units = store.source_units_at_path(path)?;
    match units.as_slice() {
        [] => Err(LocationResolutionError::UnknownPath(
            path.as_str().to_owned(),
        )),
        [unit] => Ok(unit.id.clone()),
        _ => Err(LocationResolutionError::AmbiguousPath(
            path.as_str().to_owned(),
        )),
    }
}

fn return_resolution(candidates: Vec<SymbolId>) -> Option<LocationResolution> {
    match candidates.as_slice() {
        [] => None,
        [symbol] => Some(LocationResolution::Symbol(symbol.clone())),
        _ => Some(LocationResolution::Ambiguous { candidates }),
    }
}

fn resolve_declaration_candidates(
    mut candidates: Vec<crate::SymbolRecord>,
) -> Option<LocationResolution> {
    let narrowest = candidates
        .iter()
        .map(|symbol| symbol.name_range.bytes.end - symbol.name_range.bytes.start)
        .min()?;
    candidates
        .retain(|symbol| symbol.name_range.bytes.end - symbol.name_range.bytes.start == narrowest);
    if candidates
        .iter()
        .any(|symbol| symbol.kind != SymbolKind::Constructor)
    {
        candidates.retain(|symbol| symbol.kind != SymbolKind::Constructor);
    }
    let mut ids = candidates
        .into_iter()
        .map(|symbol| symbol.id)
        .collect::<Vec<_>>();
    ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    ids.dedup_by(|left, right| left.as_str() == right.as_str());
    return_resolution(ids)
}

/// Converts one-based Unicode-scalar columns to a UTF-8 byte offset. The end
/// of a line is a valid cursor position; a column cannot split a scalar.
pub fn byte_offset(text: &str, position: TextPosition) -> Result<u64, LocationResolutionError> {
    if position.line == 0 || position.column == 0 {
        return Err(LocationResolutionError::InvalidCoordinate);
    }
    let mut prefix_bytes = 0;
    let line_start = text
        .split_inclusive('\n')
        .enumerate()
        .find_map(|(index, line)| {
            if index + 1 == position.line as usize {
                Some(line)
            } else {
                prefix_bytes += line.len();
                None
            }
        })
        .ok_or(LocationResolutionError::OutOfRange)?;
    let line = line_start.strip_suffix('\n').unwrap_or(line_start);
    let scalar_offset = position.column as usize - 1;
    let column_offset = if scalar_offset == line.chars().count() {
        line.len()
    } else {
        line.char_indices()
            .nth(scalar_offset)
            .map(|(offset, _)| offset)
            .ok_or(LocationResolutionError::OutOfRange)?
    };
    Ok((prefix_bytes + column_offset) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_are_one_based_unicode_scalar_columns() {
        assert_eq!(
            byte_offset("a😀b\nnext", TextPosition { line: 1, column: 3 }).expect("offset"),
            5
        );
        assert_eq!(
            byte_offset("a😀b\nnext", TextPosition { line: 2, column: 1 }).expect("offset"),
            7
        );
    }
}
