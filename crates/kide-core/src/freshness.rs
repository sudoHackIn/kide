//! Conservative planning for reusing persisted source snapshots.
//!
//! The planner has no compiler knowledge: a changed content or context
//! fingerprint always requires a fresh worker snapshot. This deliberately
//! over-invalidates instead of exposing a semantic fact from a prior input.

use std::collections::BTreeMap;

use crate::{Provenance, SourceUnit, SourceUnitId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidationReason {
    ContentChanged,
    ContextChanged,
    BackendChanged,
    AnalysisOptionsChanged,
    PublicApiChanged,
}

/// Returns the source units that must be conservatively reanalyzed after a
/// completed snapshot changed its public API. Body-only changes deliberately
/// return no dependents, preserving the narrow incremental fast path.
pub fn api_dependent_invalidations(
    previous_api: Option<&crate::Fingerprint>,
    current_api: Option<&crate::Fingerprint>,
    dependents: impl IntoIterator<Item = SourceUnitId>,
) -> Vec<SourceUnitId> {
    if previous_api == current_api {
        return Vec::new();
    }
    let mut dependents = dependents.into_iter().collect::<Vec<_>>();
    dependents.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    dependents.dedup_by(|left, right| left.as_str() == right.as_str());
    dependents
}

/// Inputs that can additionally detect a worker implementation or option drift.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisInput {
    pub source_unit: SourceUnit,
    pub provenance: Provenance,
    pub public_api_fingerprint: Option<crate::Fingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexAction {
    Reuse(SourceUnit),
    Reanalyze {
        source_unit: SourceUnit,
        reason: InvalidationReason,
    },
    Remove {
        source_unit: SourceUnitId,
    },
}

/// Computes a deterministic, conservative source-level update plan.
pub fn plan_invalidation(current: &[SourceUnit], persisted: &[SourceUnit]) -> Vec<IndexAction> {
    let current = current
        .iter()
        .cloned()
        .map(|unit| (unit.id.as_str().to_owned(), unit))
        .collect::<BTreeMap<_, _>>();
    let persisted = persisted
        .iter()
        .cloned()
        .map(|unit| (unit.id.as_str().to_owned(), unit))
        .collect::<BTreeMap<_, _>>();

    let mut actions = Vec::with_capacity(current.len() + persisted.len());
    for (id, unit) in &current {
        match persisted.get(id) {
            None => actions.push(IndexAction::Reanalyze {
                source_unit: unit.clone(),
                reason: InvalidationReason::ContentChanged,
            }),
            Some(previous) if previous.context != unit.context => {
                actions.push(IndexAction::Reanalyze {
                    source_unit: unit.clone(),
                    reason: InvalidationReason::ContextChanged,
                })
            }
            Some(previous) if previous.content != unit.content => {
                actions.push(IndexAction::Reanalyze {
                    source_unit: unit.clone(),
                    reason: InvalidationReason::ContentChanged,
                })
            }
            Some(_) => actions.push(IndexAction::Reuse(unit.clone())),
        }
    }
    for id in persisted.keys().filter(|id| !current.contains_key(*id)) {
        actions.push(IndexAction::Remove {
            source_unit: SourceUnitId::new((*id).clone()),
        });
    }
    actions
}

/// Extends source comparison with worker identity and analysis-option checks.
pub fn plan_analysis_invalidation(
    current: &[AnalysisInput],
    persisted: &[AnalysisInput],
) -> Vec<IndexAction> {
    let current = current
        .iter()
        .map(|input| (input.source_unit.id.as_str(), input))
        .collect::<BTreeMap<_, _>>();
    let persisted = persisted
        .iter()
        .map(|input| (input.source_unit.id.as_str(), input))
        .collect::<BTreeMap<_, _>>();
    let mut actions = Vec::with_capacity(current.len() + persisted.len());
    for (id, input) in &current {
        let source_unit = input.source_unit.clone();
        let reason = match persisted.get(id) {
            None => Some(InvalidationReason::ContentChanged),
            Some(previous) if previous.source_unit.context != source_unit.context => {
                Some(InvalidationReason::ContextChanged)
            }
            Some(previous) if previous.source_unit.content != source_unit.content => {
                Some(InvalidationReason::ContentChanged)
            }
            Some(previous)
                if previous.provenance.backend != input.provenance.backend
                    || previous.provenance.backend_version != input.provenance.backend_version
                    || previous.provenance.protocol_version
                        != input.provenance.protocol_version =>
            {
                Some(InvalidationReason::BackendChanged)
            }
            Some(previous)
                if previous.provenance.analysis_options != input.provenance.analysis_options =>
            {
                Some(InvalidationReason::AnalysisOptionsChanged)
            }
            Some(_) => None,
        };
        actions.push(match reason {
            Some(reason) => IndexAction::Reanalyze {
                source_unit,
                reason,
            },
            None => IndexAction::Reuse(source_unit),
        });
    }
    for id in persisted.keys().filter(|id| !current.contains_key(*id)) {
        actions.push(IndexAction::Remove {
            source_unit: SourceUnitId::new(*id),
        });
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ComponentId, Fingerprint, Language, SourceOrigin, WorkspacePath};

    fn unit(id: &str, content: &str, context: &str) -> SourceUnit {
        SourceUnit {
            id: SourceUnitId::new(id),
            component: ComponentId::new("fixture:main"),
            path: WorkspacePath::new(format!("src/{id}.kt")),
            language: Language::Kotlin,
            origin: SourceOrigin::Source,
            content: Fingerprint::new(content),
            context: Fingerprint::new(context),
        }
    }

    #[test]
    fn reuses_only_identical_inputs_and_removes_deleted_units() {
        let unchanged = unit("unchanged", "sha256:a", "sha256:context");
        let body_edit = unit("body", "sha256:new", "sha256:context");
        let config_edit = unit("config", "sha256:a", "sha256:new-context");
        assert_eq!(
            plan_invalidation(
                &[unchanged.clone(), body_edit.clone(), config_edit.clone()],
                &[
                    unchanged.clone(),
                    unit("body", "sha256:old", "sha256:context"),
                    unit("config", "sha256:a", "sha256:old-context"),
                    unit("deleted", "sha256:a", "sha256:context"),
                ],
            ),
            vec![
                IndexAction::Reanalyze {
                    source_unit: body_edit,
                    reason: InvalidationReason::ContentChanged
                },
                IndexAction::Reanalyze {
                    source_unit: config_edit,
                    reason: InvalidationReason::ContextChanged
                },
                IndexAction::Reuse(unchanged),
                IndexAction::Remove {
                    source_unit: SourceUnitId::new("deleted")
                },
            ],
        );
    }

    #[test]
    fn invalidates_when_backend_or_analysis_options_change() {
        let source = unit("source", "sha256:a", "sha256:context");
        let current = AnalysisInput {
            source_unit: source.clone(),
            provenance: provenance("1", "sha256:options"),
            public_api_fingerprint: None,
        };
        let old_backend = AnalysisInput {
            source_unit: source.clone(),
            provenance: provenance("0", "sha256:options"),
            public_api_fingerprint: None,
        };
        let old_options = AnalysisInput {
            source_unit: source.clone(),
            provenance: provenance("1", "sha256:old-options"),
            public_api_fingerprint: None,
        };
        assert!(matches!(
            plan_analysis_invalidation(std::slice::from_ref(&current), &[old_backend])[..],
            [IndexAction::Reanalyze {
                reason: InvalidationReason::BackendChanged,
                ..
            }]
        ));
        assert!(matches!(
            plan_analysis_invalidation(&[current], &[old_options])[..],
            [IndexAction::Reanalyze {
                reason: InvalidationReason::AnalysisOptionsChanged,
                ..
            }]
        ));
    }

    #[test]
    fn api_change_invalidates_sorted_dependents_but_body_only_does_not() {
        let old = Fingerprint::new("sha256:old-api");
        let new = Fingerprint::new("sha256:new-api");
        assert!(
            api_dependent_invalidations(Some(&old), Some(&old), [SourceUnitId::new("b")])
                .is_empty()
        );
        assert_eq!(
            api_dependent_invalidations(
                Some(&old),
                Some(&new),
                [
                    SourceUnitId::new("b"),
                    SourceUnitId::new("a"),
                    SourceUnitId::new("b")
                ]
            ),
            vec![SourceUnitId::new("a"), SourceUnitId::new("b")],
        );
    }

    fn provenance(version: &str, options: &str) -> Provenance {
        Provenance {
            backend: "fixture-worker".to_owned(),
            backend_version: version.to_owned(),
            protocol_version: 2,
            analysis_options: Fingerprint::new(options),
        }
    }
}
