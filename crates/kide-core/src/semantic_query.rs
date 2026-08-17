//! Typed, bounded semantic-query IR compiled to persistent selector primitives.
//!
//! Textual package syntax and manifests deliberately live above this module.
//! The Core boundary receives only typed programs and parameter values, so a
//! project command cannot splice arbitrary text into an executable query.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::{
    selector::{select, Selector, SelectorError, SelectorPredicate, SelectorState},
    ComponentId, IndexStore, IndexStoreError, Language, SymbolId, SymbolKind, SymbolRecord,
};

/// Core policy cap. Packages may request lower limits, never a larger result
/// set; candidate/node/byte/deadline caps are added with relation traversal.
pub const MAX_RESULT_LIMIT: u32 = 1_000;

/// A parsed declarative program after package/project command resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryProgram {
    pub from: QueryFrom,
    pub predicates: Vec<QueryPredicate>,
    pub limit: u32,
}

/// v1 requires an indexed positive starting relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryFrom {
    AppliedSymbol(QuerySymbol),
}

/// A conjunction of filters applied after the bounded starting posting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryPredicate {
    Kind(SymbolKind),
    Language(Language),
    Component(QueryComponent),
    QualifiedNamePrefix(QueryString),
}

/// Symbol values may be direct stable IDs, exact qualified names, or typed
/// parameter references. Exact qualified names resolve uniquely in the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuerySymbol {
    Id(SymbolId),
    QualifiedName(String),
    Parameter(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryComponent {
    Id(ComponentId),
    Parameter(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryString {
    Literal(String),
    Parameter(String),
}

/// Values are bound structurally by the package command frontend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryValue {
    SymbolId(SymbolId),
    QualifiedSymbol(String),
    ComponentId(ComponentId),
    String(String),
    Integer(u32),
}

pub type QueryParameters = BTreeMap<String, QueryValue>;

/// Stable, inspectable representation of the plan Core actually executes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledQuery {
    pub selector: Selector,
    pub starting_symbol: SymbolId,
    pub limit: u32,
}

/// Query execution result. `Partial` means the explicit output limit truncated
/// an otherwise valid bounded posting, not that no matching record exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryResult {
    pub state: SelectorState,
    pub plan: CompiledQuery,
    pub symbols: Vec<SymbolRecord>,
}

#[derive(Debug, Error)]
pub enum QueryCompileError {
    #[error("query limit must be between 1 and {MAX_RESULT_LIMIT}")]
    InvalidLimit,
    #[error("query parameter `{0}` is required")]
    MissingParameter(String),
    #[error("query parameter `{name}` must be {expected}")]
    InvalidParameterType {
        name: String,
        expected: &'static str,
    },
    #[error("qualified symbol `{0}` did not resolve")]
    UnknownQualifiedSymbol(String),
    #[error("qualified symbol `{name}` is ambiguous: {count} candidates")]
    AmbiguousQualifiedSymbol { name: String, count: usize },
    #[error("index lookup failed: {0}")]
    Store(#[from] IndexStoreError),
    #[error("selector execution failed: {0}")]
    Selector(#[from] SelectorError),
}

/// Resolves typed parameters and compiles a program to the existing bounded
/// selector. No scan is possible: `AppliedSymbol` is mandatory in `QueryFrom`.
pub fn compile(
    store: &IndexStore,
    program: &QueryProgram,
    parameters: &QueryParameters,
) -> Result<CompiledQuery, QueryCompileError> {
    if program.limit == 0 || program.limit > MAX_RESULT_LIMIT {
        return Err(QueryCompileError::InvalidLimit);
    }
    let starting_symbol = match &program.from {
        QueryFrom::AppliedSymbol(symbol) => resolve_symbol(store, symbol, parameters)?,
    };
    let mut predicates = vec![SelectorPredicate::AppliedSymbol(starting_symbol.clone())];
    for predicate in &program.predicates {
        predicates.push(match predicate {
            QueryPredicate::Kind(kind) => SelectorPredicate::Kind(kind.clone()),
            QueryPredicate::Language(language) => SelectorPredicate::Language(language.clone()),
            QueryPredicate::Component(component) => {
                SelectorPredicate::Component(resolve_component(component, parameters)?)
            }
            QueryPredicate::QualifiedNamePrefix(prefix) => {
                SelectorPredicate::QualifiedNamePrefix(resolve_string(prefix, parameters)?)
            }
        });
    }
    Ok(CompiledQuery {
        selector: Selector {
            views: Vec::new(),
            predicates,
        },
        starting_symbol,
        limit: program.limit,
    })
}

/// Executes a compiled program and makes output truncation explicit.
pub fn execute(
    store: &IndexStore,
    program: &QueryProgram,
    parameters: &QueryParameters,
) -> Result<QueryResult, QueryCompileError> {
    let plan = compile(store, program, parameters)?;
    let mut selected = select(store, &plan.selector)?;
    let truncated = selected.symbols.len() > plan.limit as usize;
    selected.symbols.truncate(plan.limit as usize);
    Ok(QueryResult {
        state: if truncated {
            SelectorState::Partial
        } else {
            selected.state
        },
        plan,
        symbols: selected.symbols,
    })
}

fn resolve_symbol(
    store: &IndexStore,
    value: &QuerySymbol,
    parameters: &QueryParameters,
) -> Result<SymbolId, QueryCompileError> {
    match value {
        QuerySymbol::Id(value) => Ok(value.clone()),
        QuerySymbol::QualifiedName(value) => resolve_qualified_symbol(store, value),
        QuerySymbol::Parameter(name) => match parameter(parameters, name)? {
            QueryValue::SymbolId(value) => Ok(value.clone()),
            QueryValue::QualifiedSymbol(value) => resolve_qualified_symbol(store, value),
            _ => Err(invalid_type(name, "symbol-id or qualified-symbol")),
        },
    }
}

fn resolve_qualified_symbol(store: &IndexStore, name: &str) -> Result<SymbolId, QueryCompileError> {
    match store.symbols_with_qualified_name(name)?.as_slice() {
        // External JVM declarations may be referenced by source facts before
        // their dependency JAR is materialized. Their canonical type identity
        // is still a valid bounded posting key.
        [] if name.contains('.') => Ok(SymbolId::new(format!("jvm:type:{name}"))),
        [] => Err(QueryCompileError::UnknownQualifiedSymbol(name.to_owned())),
        [symbol] => Ok(symbol.clone()),
        values => Err(QueryCompileError::AmbiguousQualifiedSymbol {
            name: name.to_owned(),
            count: values.len(),
        }),
    }
}

fn resolve_component(
    value: &QueryComponent,
    parameters: &QueryParameters,
) -> Result<ComponentId, QueryCompileError> {
    match value {
        QueryComponent::Id(value) => Ok(value.clone()),
        QueryComponent::Parameter(name) => match parameter(parameters, name)? {
            QueryValue::ComponentId(value) => Ok(value.clone()),
            _ => Err(invalid_type(name, "component-id")),
        },
    }
}

fn resolve_string(
    value: &QueryString,
    parameters: &QueryParameters,
) -> Result<String, QueryCompileError> {
    match value {
        QueryString::Literal(value) => Ok(value.clone()),
        QueryString::Parameter(name) => match parameter(parameters, name)? {
            QueryValue::String(value) => Ok(value.clone()),
            _ => Err(invalid_type(name, "string")),
        },
    }
}

fn parameter<'a>(
    parameters: &'a QueryParameters,
    name: &str,
) -> Result<&'a QueryValue, QueryCompileError> {
    parameters
        .get(name)
        .ok_or_else(|| QueryCompileError::MissingParameter(name.to_owned()))
}

fn invalid_type(name: &str, expected: &'static str) -> QueryCompileError {
    QueryCompileError::InvalidParameterType {
        name: name.to_owned(),
        expected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_type_errors_are_explicit() {
        let values =
            QueryParameters::from([("component".to_owned(), QueryValue::String("app".to_owned()))]);
        assert!(matches!(
            resolve_component(&QueryComponent::Parameter("component".to_owned()), &values),
            Err(QueryCompileError::InvalidParameterType { .. })
        ));
    }

    #[test]
    fn result_limit_is_bounded_by_core_policy() {
        let program = QueryProgram {
            from: QueryFrom::AppliedSymbol(QuerySymbol::Id(SymbolId::new("annotation"))),
            predicates: Vec::new(),
            limit: MAX_RESULT_LIMIT + 1,
        };
        assert_eq!(program.limit, 1_001);
    }
}
