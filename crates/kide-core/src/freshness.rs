//! Conservative planning for reusing persisted source snapshots.
//!
//! The planner has no compiler knowledge: a changed content or context
//! fingerprint always requires a fresh worker snapshot. This deliberately
//! over-invalidates instead of exposing a semantic fact from a prior input.

use std::collections::BTreeMap;

use crate::{SourceUnit, SourceUnitId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidationReason {
    ContentChanged,
    ContextChanged,
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
            source_unit: SourceUnitId::new(id),
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
}
