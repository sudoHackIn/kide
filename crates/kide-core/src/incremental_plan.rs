//! Pure, explainable planning for an incremental index generation.
//!
//! Build resolution is deliberately outside this module: callers first decide
//! whether a persisted manifest may be reused, then pass its resolved artifact
//! inventory here.  That keeps the no-worker fast path independent from JVM
//! tooling while making every subsequent source and cache decision testable.

use std::collections::BTreeMap;

use crate::{
    AnalysisInput, ArtifactBlobKey, ArtifactDescriptor, IndexAction, SourceUnit,
    plan_analysis_invalidation, plan_invalidation,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestAction {
    Reuse,
    Resolve,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyAction {
    CacheHit(ArtifactDescriptor),
    CacheMiss(ArtifactDescriptor),
    Removed(ArtifactDescriptor),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncrementalIndexPlan {
    pub manifest: ManifestAction,
    pub sources: Vec<IndexAction>,
    pub dependencies: Vec<DependencyAction>,
}

impl IncrementalIndexPlan {
    pub fn requires_source_worker(&self) -> bool {
        self.sources
            .iter()
            .any(|action| matches!(action, IndexAction::Reanalyze { .. }))
    }

    pub fn dependency_misses(&self) -> impl Iterator<Item = &ArtifactDescriptor> {
        self.dependencies.iter().filter_map(|action| match action {
            DependencyAction::CacheMiss(descriptor) => Some(descriptor),
            _ => None,
        })
    }
}

pub fn plan_incremental_index(
    manifest: ManifestAction,
    current_sources: &[SourceUnit],
    persisted_sources: &[SourceUnit],
    current_dependencies: &[ArtifactDescriptor],
    persisted_dependencies: &[ArtifactDescriptor],
    blob_is_available: impl FnMut(&ArtifactBlobKey) -> bool,
) -> IncrementalIndexPlan {
    plan_with_source_actions(
        manifest,
        plan_invalidation(current_sources, persisted_sources),
        current_dependencies,
        persisted_dependencies,
        blob_is_available,
    )
}

/// Variant for a source worker whose backend identity and analysis options are
/// part of the invalidation boundary.
pub fn plan_incremental_analysis_index(
    manifest: ManifestAction,
    current_sources: &[AnalysisInput],
    persisted_sources: &[AnalysisInput],
    current_dependencies: &[ArtifactDescriptor],
    persisted_dependencies: &[ArtifactDescriptor],
    blob_is_available: impl FnMut(&ArtifactBlobKey) -> bool,
) -> IncrementalIndexPlan {
    plan_with_source_actions(
        manifest,
        plan_analysis_invalidation(current_sources, persisted_sources),
        current_dependencies,
        persisted_dependencies,
        blob_is_available,
    )
}

fn plan_with_source_actions(
    manifest: ManifestAction,
    sources: Vec<IndexAction>,
    current_dependencies: &[ArtifactDescriptor],
    persisted_dependencies: &[ArtifactDescriptor],
    mut blob_is_available: impl FnMut(&ArtifactBlobKey) -> bool,
) -> IncrementalIndexPlan {
    let current = current_dependencies
        .iter()
        .map(|descriptor| (descriptor.source_unit.id.as_str(), descriptor))
        .collect::<BTreeMap<_, _>>();
    let persisted = persisted_dependencies
        .iter()
        .map(|descriptor| (descriptor.source_unit.id.as_str(), descriptor))
        .collect::<BTreeMap<_, _>>();
    let mut dependencies = Vec::with_capacity(current.len() + persisted.len());
    for (id, descriptor) in &current {
        let unchanged = persisted
            .get(id)
            .is_some_and(|previous| previous.resolved_identity() == descriptor.resolved_identity());
        let key = ArtifactBlobKey::from_identity(descriptor.resolved_identity());
        dependencies.push(if unchanged && blob_is_available(&key) {
            DependencyAction::CacheHit((*descriptor).clone())
        } else {
            DependencyAction::CacheMiss((*descriptor).clone())
        });
    }
    for (id, descriptor) in persisted {
        if !current.contains_key(id) {
            dependencies.push(DependencyAction::Removed(descriptor.clone()));
        }
    }
    IncrementalIndexPlan {
        manifest,
        sources,
        dependencies,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ComponentId, Fingerprint, Language, Provenance, SourceOrigin, SourceUnitId, WorkspacePath,
    };

    fn source(id: &str, content: &str) -> SourceUnit {
        SourceUnit {
            id: SourceUnitId::new(id),
            component: ComponentId::new("fixture:main"),
            path: WorkspacePath::new(format!("src/{id}.java")),
            language: Language::Java,
            origin: SourceOrigin::Source,
            content: Fingerprint::new(content),
            context: Fingerprint::new("sha256:context"),
        }
    }
    fn dependency(content: &str) -> ArtifactDescriptor {
        ArtifactDescriptor {
            source_unit: SourceUnit {
                id: SourceUnitId::new("jvm:fixture"),
                component: ComponentId::new("fixture:main"),
                path: WorkspacePath::new("fixture.jar"),
                language: Language::Java,
                origin: SourceOrigin::Dependency,
                content: Fingerprint::new(content),
                context: Fingerprint::new("sha256:context"),
            },
            provenance: Provenance {
                backend: "fixture".into(),
                backend_version: "1".into(),
                protocol_version: 3,
                analysis_options: Fingerprint::new("sha256:options"),
            },
            resolved_identity: None,
            symbol_locators: Vec::new(),
        }
    }

    #[test]
    fn warm_plan_reuses_sources_and_exact_dependency_blob() {
        let source = source("A", "sha256:a");
        let dependency = dependency("sha256:jar");
        let plan = plan_incremental_index(
            ManifestAction::Reuse,
            std::slice::from_ref(&source),
            std::slice::from_ref(&source),
            std::slice::from_ref(&dependency),
            std::slice::from_ref(&dependency),
            |_| true,
        );
        assert!(!plan.requires_source_worker());
        assert!(matches!(
            plan.dependencies.as_slice(),
            [DependencyAction::CacheHit(_)]
        ));
    }

    #[test]
    fn source_or_dependency_identity_change_is_narrow() {
        let old_source = source("A", "sha256:old");
        let new_source = source("A", "sha256:new");
        let plan = plan_incremental_index(
            ManifestAction::Resolve,
            &[new_source],
            &[old_source],
            &[dependency("sha256:new")],
            &[dependency("sha256:old")],
            |_| true,
        );
        assert!(plan.requires_source_worker());
        assert!(matches!(
            plan.dependencies.as_slice(),
            [DependencyAction::CacheMiss(_)]
        ));
    }

    #[test]
    fn missing_blob_is_a_miss_even_when_identity_matches() {
        let dependency = dependency("sha256:jar");
        let plan = plan_incremental_index(
            ManifestAction::Reuse,
            &[],
            &[],
            std::slice::from_ref(&dependency),
            std::slice::from_ref(&dependency),
            |_| false,
        );
        assert_eq!(plan.dependency_misses().count(), 1);
    }

    #[test]
    fn worker_provenance_drift_is_a_dependency_miss() {
        let previous = dependency("sha256:jar");
        let mut current = previous.clone();
        current.provenance.backend_version = "2".into();
        let plan = plan_incremental_index(
            ManifestAction::Resolve,
            &[],
            &[],
            std::slice::from_ref(&current),
            std::slice::from_ref(&previous),
            |_| true,
        );
        assert_eq!(plan.dependency_misses().count(), 1);
    }
}
