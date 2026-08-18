//! Framework-neutral, explainable candidate result records.
//!
//! Core owns the stable envelope and validation rules, while packages and
//! workers own namespaced reason/evidence codes such as `spring.beans.primary`.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Provenance, SourceRange, SymbolId, SymbolRecord};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameworkQueryState {
    Complete,
    Partial,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateDisposition {
    Selected,
    Excluded,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FrameworkEvidenceTarget {
    Symbol { symbol: SymbolId },
    Source { range: SourceRange },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameworkEvidence {
    /// Package-owned namespaced code, never interpreted by Core.
    pub code: String,
    pub target: FrameworkEvidenceTarget,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameworkAssumption {
    /// Package-owned namespaced environment key.
    pub code: String,
    pub value: Option<String>,
    pub evidence: Vec<FrameworkEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameworkBoundary {
    /// Package-owned namespaced reason for a partial or unsupported answer.
    pub code: String,
    pub message: String,
    pub evidence: Vec<FrameworkEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameworkCandidate {
    pub symbol: SymbolRecord,
    pub disposition: CandidateDisposition,
    pub reasons: Vec<String>,
    pub evidence: Vec<FrameworkEvidence>,
    pub assumptions: Vec<FrameworkAssumption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameworkQueryResult {
    pub state: FrameworkQueryState,
    pub candidates: Vec<FrameworkCandidate>,
    pub boundaries: Vec<FrameworkBoundary>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FrameworkQueryResultError {
    #[error("framework code `{0}` must be a lower-case namespaced identifier")]
    InvalidCode(String),
    #[error("framework candidate `{0}` has no reason codes")]
    MissingReasons(String),
    #[error("framework candidate `{0}` has no source evidence")]
    MissingEvidence(String),
    #[error("duplicate framework candidate `{0}`")]
    DuplicateCandidate(String),
    #[error("a complete framework result cannot contain unresolved candidates or boundaries")]
    InvalidCompleteState,
    #[error("a partial framework result must name at least one evidence boundary")]
    MissingPartialBoundary,
    #[error("an unsupported framework result must contain only named boundaries")]
    InvalidUnsupportedState,
}

impl FrameworkQueryResult {
    /// Validates the public contract and canonicalizes every repeated field so
    /// equivalent package/worker output has byte-stable JSON ordering.
    pub fn normalize(mut self) -> Result<Self, FrameworkQueryResultError> {
        for candidate in &mut self.candidates {
            normalize_candidate(candidate)?;
        }
        self.candidates
            .sort_by(|left, right| left.symbol.id.as_str().cmp(right.symbol.id.as_str()));
        if self
            .candidates
            .windows(2)
            .any(|pair| pair[0].symbol.id.as_str() == pair[1].symbol.id.as_str())
        {
            return Err(FrameworkQueryResultError::DuplicateCandidate(
                self.candidates
                    .windows(2)
                    .find(|pair| pair[0].symbol.id.as_str() == pair[1].symbol.id.as_str())
                    .expect("duplicate pair exists")[0]
                    .symbol
                    .id
                    .as_str()
                    .to_owned(),
            ));
        }
        for boundary in &mut self.boundaries {
            validate_code(&boundary.code)?;
            normalize_evidence(&mut boundary.evidence)?;
        }
        self.boundaries
            .sort_by(|left, right| (&left.code, &left.message).cmp(&(&right.code, &right.message)));
        match self.state {
            FrameworkQueryState::Complete
                if !self.boundaries.is_empty()
                    || self.candidates.iter().any(|candidate| {
                        candidate.disposition == CandidateDisposition::Unresolved
                    }) =>
            {
                Err(FrameworkQueryResultError::InvalidCompleteState)
            }
            FrameworkQueryState::Partial if self.boundaries.is_empty() => {
                Err(FrameworkQueryResultError::MissingPartialBoundary)
            }
            FrameworkQueryState::Unsupported
                if self.boundaries.is_empty() || !self.candidates.is_empty() =>
            {
                Err(FrameworkQueryResultError::InvalidUnsupportedState)
            }
            _ => Ok(self),
        }
    }
}

fn normalize_candidate(
    candidate: &mut FrameworkCandidate,
) -> Result<(), FrameworkQueryResultError> {
    if candidate.reasons.is_empty() {
        return Err(FrameworkQueryResultError::MissingReasons(
            candidate.symbol.id.as_str().to_owned(),
        ));
    }
    for reason in &candidate.reasons {
        validate_code(reason)?;
    }
    candidate.reasons.sort();
    candidate.reasons.dedup();
    if candidate.evidence.is_empty() {
        return Err(FrameworkQueryResultError::MissingEvidence(
            candidate.symbol.id.as_str().to_owned(),
        ));
    }
    normalize_evidence(&mut candidate.evidence)?;
    for assumption in &mut candidate.assumptions {
        validate_code(&assumption.code)?;
        normalize_evidence(&mut assumption.evidence)?;
    }
    candidate
        .assumptions
        .sort_by(|left, right| (&left.code, &left.value).cmp(&(&right.code, &right.value)));
    Ok(())
}

fn normalize_evidence(
    evidence: &mut Vec<FrameworkEvidence>,
) -> Result<(), FrameworkQueryResultError> {
    for item in evidence.iter() {
        validate_code(&item.code)?;
    }
    evidence.sort_by_key(evidence_key);
    let mut seen = BTreeSet::new();
    evidence.retain(|item| seen.insert(evidence_key(item)));
    Ok(())
}

fn evidence_key(evidence: &FrameworkEvidence) -> String {
    let target = match &evidence.target {
        FrameworkEvidenceTarget::Symbol { symbol } => format!("symbol:{}", symbol.as_str()),
        FrameworkEvidenceTarget::Source { range } => format!(
            "source:{}:{}:{}",
            range.source_unit.as_str(),
            range.bytes.start,
            range.bytes.end
        ),
    };
    format!(
        "{}|{}|{}|{}|{}",
        evidence.code,
        target,
        evidence.provenance.backend,
        evidence.provenance.backend_version,
        evidence.provenance.analysis_options.as_str()
    )
}

fn validate_code(code: &str) -> Result<(), FrameworkQueryResultError> {
    let valid = code.contains('.')
        && code.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(FrameworkQueryResultError::InvalidCode(code.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BackendKey, ByteRange, Completeness, ComponentId, Fingerprint, Freshness, Language,
        SourceRange, SourceUnitId, SymbolKind, WORKER_PROTOCOL_VERSION,
    };

    #[test]
    fn conditional_candidate_results_are_deterministic_and_explainable() {
        let mut second = candidate("bean:b", CandidateDisposition::Unresolved);
        second.assumptions.push(FrameworkAssumption {
            code: "spring.environment.property".to_owned(),
            value: Some("feature.books=true".to_owned()),
            evidence: vec![evidence("spring.condition.declaration", 8, 30)],
        });
        let normalized = FrameworkQueryResult {
            state: FrameworkQueryState::Partial,
            candidates: vec![
                candidate("bean:c", CandidateDisposition::Excluded),
                second,
                candidate("bean:a", CandidateDisposition::Selected),
            ],
            boundaries: vec![FrameworkBoundary {
                code: "spring.environment.unknown-property".to_owned(),
                message: "feature.books is not fixed by the query environment".to_owned(),
                evidence: vec![evidence("spring.condition.declaration", 8, 30)],
            }],
        }
        .normalize()
        .expect("normalizes explainable result");

        assert_eq!(normalized.candidates[0].symbol.id.as_str(), "bean:a");
        assert_eq!(normalized.candidates[1].symbol.id.as_str(), "bean:b");
        assert_eq!(
            normalized.candidates[2].disposition,
            CandidateDisposition::Excluded
        );
        let json = serde_json::to_value(normalized).expect("serializes result");
        assert_eq!(json["state"], "partial");
        assert_eq!(
            json["candidates"][1]["assumptions"][0]["code"],
            "spring.environment.property"
        );
    }

    #[test]
    fn rejects_false_complete_and_unnamed_unsupported_results() {
        assert_eq!(
            FrameworkQueryResult {
                state: FrameworkQueryState::Complete,
                candidates: vec![candidate("bean:a", CandidateDisposition::Unresolved)],
                boundaries: Vec::new(),
            }
            .normalize(),
            Err(FrameworkQueryResultError::InvalidCompleteState)
        );
        assert_eq!(
            FrameworkQueryResult {
                state: FrameworkQueryState::Unsupported,
                candidates: Vec::new(),
                boundaries: Vec::new(),
            }
            .normalize(),
            Err(FrameworkQueryResultError::InvalidUnsupportedState)
        );
    }

    fn candidate(id: &str, disposition: CandidateDisposition) -> FrameworkCandidate {
        FrameworkCandidate {
            symbol: SymbolRecord {
                id: SymbolId::new(id),
                backend_key: BackendKey {
                    backend: "fixture".to_owned(),
                    schema_version: 1,
                    value: id.to_owned(),
                },
                language: Language::Kotlin,
                kind: SymbolKind::Class,
                name: id.to_owned(),
                qualified_name: Some(id.to_owned()),
                signature: None,
                component: ComponentId::new("fixture:main"),
                declaration: range(0, 40),
                name_range: range(6, 12),
                owner: None,
                modifiers: Vec::new(),
                applied_symbols: Vec::new(),
                freshness: Freshness::Fresh,
                completeness: Completeness::Complete,
                provenance: provenance(),
            },
            disposition,
            reasons: vec!["spring.beans.type-compatible".to_owned()],
            evidence: vec![evidence("spring.beans.declaration", 0, 40)],
            assumptions: Vec::new(),
        }
    }

    fn evidence(code: &str, start: u64, end: u64) -> FrameworkEvidence {
        FrameworkEvidence {
            code: code.to_owned(),
            target: FrameworkEvidenceTarget::Source {
                range: range(start, end),
            },
            provenance: provenance(),
        }
    }

    fn range(start: u64, end: u64) -> SourceRange {
        SourceRange {
            source_unit: SourceUnitId::new("fixture:src/App.kt"),
            bytes: ByteRange { start, end },
        }
    }

    fn provenance() -> Provenance {
        Provenance {
            backend: "fixture".to_owned(),
            backend_version: "1.0.0".to_owned(),
            protocol_version: WORKER_PROTOCOL_VERSION,
            analysis_options: Fingerprint::new("sha256:fixture"),
        }
    }
}
