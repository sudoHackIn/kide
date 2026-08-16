//! Bounded, framework-neutral semantic selector planning.

use thiserror::Error;

use crate::{
    ApplicationValue, Completeness, ComponentId, Freshness, IndexStore, IndexStoreError, Language,
    ResultMetadata, SelectorRecord, SymbolId, SymbolKind, SymbolRecord,
};

/// A language-level convenience view. It compiles to canonical predicates;
/// framework names never enter Core's selector surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageView {
    KotlinClass,
    JavaClass,
}

/// A composable predicate over canonical declaration facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorPredicate {
    Kind(SymbolKind),
    AppliedSymbol(SymbolId),
    QualifiedNamePrefix(String),
    Language(Language),
    Component(ComponentId),
    Backend(String),
}

/// A conjunction of predicates and reusable language views.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selector {
    pub views: Vec<LanguageView>,
    pub predicates: Vec<SelectorPredicate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorPlan {
    AppliedSymbolPosting { applied_symbol: SymbolId },
}

/// A bounded composed application pattern. The names are relations, not
/// framework concepts: callers supply resolved target identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NestedApplicationArgumentPattern {
    pub nested_target: SymbolId,
    pub outer_target: SymbolId,
    pub argument_name: String,
    pub argument_value: ApplicationValue,
}

/// Explicit answer state: a partial snapshot is never reported as complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorState {
    Complete,
    Partial,
    NoResult,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorResult {
    pub state: SelectorState,
    pub plan: SelectorPlan,
    pub symbols: Vec<SymbolRecord>,
}

#[derive(Debug, Error)]
pub enum SelectorError {
    #[error("selector requires a resolved annotation predicate to bound its indexed starting set")]
    Unbounded,
    #[error("index lookup failed: {0}")]
    Store(#[from] IndexStoreError),
}

/// Compiles and executes a bounded selector. The resolved applied-symbol posting
/// is mandatory for this MVP, so evaluation never starts by decoding every
/// symbol blob. Remaining predicates are applied only to posting candidates.
pub fn select(store: &IndexStore, selector: &Selector) -> Result<SelectorResult, SelectorError> {
    let predicates = normalized_predicates(selector);
    let annotation = predicates
        .iter()
        .filter_map(|predicate| match predicate {
            SelectorPredicate::AppliedSymbol(symbol) => Some(symbol.clone()),
            _ => None,
        })
        .min_by(|left, right| left.as_str().cmp(right.as_str()))
        .ok_or(SelectorError::Unbounded)?;
    let plan = SelectorPlan::AppliedSymbolPosting {
        applied_symbol: annotation.clone(),
    };
    let mut symbols = store.symbols_with_applied_symbol(&annotation)?;
    symbols.retain(|symbol| {
        predicates
            .iter()
            .all(|predicate| matches(symbol, predicate))
    });
    let state = if symbols.is_empty() {
        SelectorState::NoResult
    } else if symbols.iter().all(|symbol| {
        symbol.freshness == Freshness::Fresh && symbol.completeness == Completeness::Complete
    }) {
        SelectorState::Complete
    } else {
        SelectorState::Partial
    };
    Ok(SelectorResult {
        state,
        plan,
        symbols,
    })
}

/// Executes `nested symbol -[applies]-> target`, `nested -[owns]-> outer`,
/// and an outer application argument match. The indexed nested-target posting
/// is always the starting set; owner and argument joins are bounded to it.
pub fn select_nested_application_argument(
    store: &IndexStore,
    pattern: &NestedApplicationArgumentPattern,
) -> Result<SelectorResult, SelectorError> {
    let symbols = store.symbols_with_nested_application_argument(
        &pattern.nested_target,
        &pattern.outer_target,
        &pattern.argument_name,
        &pattern.argument_value,
    )?;
    let state = if symbols.is_empty() {
        SelectorState::NoResult
    } else if symbols.iter().all(|symbol| {
        symbol.freshness == Freshness::Fresh && symbol.completeness == Completeness::Complete
    }) {
        SelectorState::Complete
    } else {
        SelectorState::Partial
    };
    Ok(SelectorResult {
        state,
        plan: SelectorPlan::AppliedSymbolPosting {
            applied_symbol: pattern.nested_target.clone(),
        },
        symbols,
    })
}

/// Converts a selector result into independent JSONL-safe records. Each
/// record carries the selected stable identity and its own provenance.
pub fn records(result: &SelectorResult) -> Vec<SelectorRecord> {
    result
        .symbols
        .iter()
        .cloned()
        .map(|symbol| SelectorRecord {
            metadata: ResultMetadata {
                freshness: symbol.freshness,
                completeness: symbol.completeness,
                precision: crate::Precision::Exact,
                index_format_version: crate::INDEX_FORMAT_VERSION,
                source_snapshot: None,
                provenance: vec![symbol.provenance.clone()],
            },
            symbol,
        })
        .collect()
}

fn normalized_predicates(selector: &Selector) -> Vec<SelectorPredicate> {
    let mut predicates = selector.predicates.clone();
    for view in &selector.views {
        match view {
            LanguageView::KotlinClass => {
                predicates.push(SelectorPredicate::Language(Language::Kotlin));
                predicates.push(SelectorPredicate::Kind(SymbolKind::Class));
            }
            LanguageView::JavaClass => {
                predicates.push(SelectorPredicate::Language(Language::Java));
                predicates.push(SelectorPredicate::Kind(SymbolKind::Class));
            }
        }
    }
    predicates
}

fn matches(symbol: &SymbolRecord, predicate: &SelectorPredicate) -> bool {
    match predicate {
        SelectorPredicate::Kind(kind) => symbol.kind == *kind,
        SelectorPredicate::AppliedSymbol(annotation) => symbol.applied_symbols.contains(annotation),
        SelectorPredicate::QualifiedNamePrefix(prefix) => symbol
            .qualified_name
            .as_deref()
            .is_some_and(|name| name.starts_with(prefix)),
        SelectorPredicate::Language(language) => symbol.language == *language,
        SelectorPredicate::Component(component) => symbol.component == *component,
        SelectorPredicate::Backend(backend) => symbol.provenance.backend == *backend,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kotlin_class_is_only_a_canonical_predicate_bundle() {
        assert_eq!(
            normalized_predicates(&Selector {
                views: vec![LanguageView::KotlinClass],
                predicates: vec![],
            }),
            vec![
                SelectorPredicate::Language(Language::Kotlin),
                SelectorPredicate::Kind(SymbolKind::Class),
            ]
        );
    }
}
