//! Parser and typed parameter binding for workspace-local `.kql` commands.

use std::{collections::BTreeMap, fs, path::Path};

use thiserror::Error;

use crate::{
    semantic_query::{
        QueryComponent, QueryFrom, QueryParameters, QueryPredicate, QueryProgram, QueryString,
        QuerySymbol, QueryValue,
    },
    ComponentId, Language, SymbolId, SymbolKind,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectQuery {
    pub name: String,
    pub parameters: Vec<QueryParameter>,
    pub program: QueryProgram,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryParameter {
    pub name: String,
    pub ty: QueryParameterType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryParameterType {
    SymbolId,
    QualifiedSymbol,
    ComponentId,
    String,
    Integer,
}

#[derive(Debug, Error)]
pub enum ProjectQueryError {
    #[error("failed to read project query `{path}`: {source}")]
    Read {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("invalid project query: {0}")]
    Invalid(String),
    #[error("unknown query parameter `{0}")]
    UnknownParameter(String),
    #[error("query parameter `{0}` is required")]
    MissingParameter(String),
    #[error("query parameter `{name}` must be {expected}")]
    InvalidParameter {
        name: String,
        expected: &'static str,
    },
}

impl ProjectQuery {
    pub fn load(path: &Path) -> Result<Self, ProjectQueryError> {
        let text = fs::read_to_string(path).map_err(|source| ProjectQueryError::Read {
            path: path.into(),
            source,
        })?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, ProjectQueryError> {
        let lines = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect::<Vec<_>>();
        let header = lines.first().ok_or_else(|| invalid("command is empty"))?;
        let rest = header
            .strip_prefix("command ")
            .and_then(|value| value.strip_suffix(" {"))
            .ok_or_else(|| invalid("expected `command name(parameters) {`"))?;
        let (name, parameters) = rest
            .split_once('(')
            .ok_or_else(|| invalid("command name needs parameters"))?;
        let parameters = parameters
            .strip_suffix(')')
            .ok_or_else(|| invalid("unterminated parameter list"))?;
        if !valid_name(name) {
            return Err(invalid("command name must be ASCII lower-case dotted"));
        }
        let parameters = parse_parameters(parameters)?;
        if lines.last().copied() != Some("}") {
            return Err(invalid("command must end with `}`"));
        }
        let mut from = None;
        let mut predicates = Vec::new();
        let mut limit = None;
        for line in &lines[1..lines.len() - 1] {
            if let Some(value) = line
                .strip_prefix("from applies(")
                .and_then(|value| value.strip_suffix(')'))
            {
                if from
                    .replace(QueryFrom::AppliedSymbol(parse_symbol(value)?))
                    .is_some()
                {
                    return Err(invalid("only one `from` is allowed"));
                }
            } else if let Some(value) = line.strip_prefix("where ") {
                predicates.extend(parse_predicates(value)?);
            } else if *line == "return symbol" {
            } else if let Some(value) = line.strip_prefix("limit ") {
                limit = Some(
                    value
                        .parse()
                        .map_err(|_| invalid("limit must be a positive integer"))?,
                );
            } else {
                return Err(invalid(format!("unsupported statement `{line}`")));
            }
        }
        let program = QueryProgram {
            from: from.ok_or_else(|| invalid("`from applies(...)` is required"))?,
            predicates,
            limit: limit.ok_or_else(|| invalid("`limit` is required"))?,
        };
        Ok(Self {
            name: name.to_owned(),
            parameters,
            program,
        })
    }

    pub fn bind(
        &self,
        values: &BTreeMap<String, String>,
    ) -> Result<QueryParameters, ProjectQueryError> {
        for name in values.keys() {
            if !self
                .parameters
                .iter()
                .any(|parameter| &parameter.name == name)
            {
                return Err(ProjectQueryError::UnknownParameter(name.clone()));
            }
        }
        self.parameters
            .iter()
            .map(|parameter| {
                let value = values
                    .get(&parameter.name)
                    .ok_or_else(|| ProjectQueryError::MissingParameter(parameter.name.clone()))?;
                let value = match parameter.ty {
                    QueryParameterType::SymbolId => QueryValue::SymbolId(SymbolId::new(value)),
                    QueryParameterType::QualifiedSymbol => {
                        QueryValue::QualifiedSymbol(value.clone())
                    }
                    QueryParameterType::ComponentId => {
                        QueryValue::ComponentId(ComponentId::new(value))
                    }
                    QueryParameterType::String => QueryValue::String(value.clone()),
                    QueryParameterType::Integer => {
                        QueryValue::Integer(value.parse().map_err(|_| {
                            ProjectQueryError::InvalidParameter {
                                name: parameter.name.clone(),
                                expected: "integer",
                            }
                        })?)
                    }
                };
                Ok((parameter.name.clone(), value))
            })
            .collect()
    }
}

fn parse_parameters(value: &str) -> Result<Vec<QueryParameter>, ProjectQueryError> {
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }
    value
        .split(',')
        .map(|value| {
            let (name, ty) = value
                .trim()
                .split_once(':')
                .ok_or_else(|| invalid("parameters use `name: type`"))?;
            if !valid_name(name.trim()) || name.contains('.') {
                return Err(invalid("parameter name must be ASCII lower-case"));
            }
            let ty = match ty.trim() {
                "symbol-id" => QueryParameterType::SymbolId,
                "qualified-symbol" => QueryParameterType::QualifiedSymbol,
                "component-id" => QueryParameterType::ComponentId,
                "string" => QueryParameterType::String,
                "integer" => QueryParameterType::Integer,
                _ => return Err(invalid("unknown parameter type")),
            };
            Ok(QueryParameter {
                name: name.trim().to_owned(),
                ty,
            })
        })
        .collect()
}

fn parse_symbol(value: &str) -> Result<QuerySymbol, ProjectQueryError> {
    if let Some(name) = value.strip_prefix('$') {
        return Ok(QuerySymbol::Parameter(name.to_owned()));
    }
    if let Some(value) = value
        .strip_prefix("symbol-id(")
        .and_then(|value| value.strip_suffix(')'))
    {
        return Ok(QuerySymbol::Id(SymbolId::new(unquote(value)?)));
    }
    if let Some(value) = value
        .strip_prefix("qualified-symbol(")
        .and_then(|value| value.strip_suffix(')'))
    {
        return Ok(QuerySymbol::QualifiedName(unquote(value)?));
    }
    Err(invalid(
        "applies accepts `$parameter`, symbol-id(\"…\"), or qualified-symbol(\"…\")",
    ))
}

fn parse_predicates(value: &str) -> Result<Vec<QueryPredicate>, ProjectQueryError> {
    value
        .split(" and ")
        .map(|term| {
            let (left, right) = term
                .split_once(" == ")
                .ok_or_else(|| invalid("where terms use `==`"))?;
            match left {
                "kind" => Ok(QueryPredicate::Kind(match right {
                    "class" => SymbolKind::Class,
                    "interface" => SymbolKind::Interface,
                    "method" => SymbolKind::Method,
                    "function" => SymbolKind::Function,
                    _ => return Err(invalid("unsupported symbol kind")),
                })),
                "language" => Ok(QueryPredicate::Language(match right {
                    "kotlin" => Language::Kotlin,
                    "java" => Language::Java,
                    _ => return Err(invalid("unsupported language")),
                })),
                "component" => Ok(QueryPredicate::Component(
                    if let Some(name) = right.strip_prefix('$') {
                        QueryComponent::Parameter(name.to_owned())
                    } else {
                        QueryComponent::Id(ComponentId::new(unquote(right)?))
                    },
                )),
                "qualified-name-prefix" => Ok(QueryPredicate::QualifiedNamePrefix(
                    if let Some(name) = right.strip_prefix('$') {
                        QueryString::Parameter(name.to_owned())
                    } else {
                        QueryString::Literal(unquote(right)?)
                    },
                )),
                _ => Err(invalid("unsupported where predicate")),
            }
        })
        .collect()
}

fn unquote(value: &str) -> Result<String, ProjectQueryError> {
    value
        .trim()
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .map(str::to_owned)
        .ok_or_else(|| invalid("string literals must be quoted"))
}
fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}
fn invalid(message: impl Into<String>) -> ProjectQueryError {
    ProjectQueryError::Invalid(message.into())
}
