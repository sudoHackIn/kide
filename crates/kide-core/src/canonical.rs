use serde::{Deserialize, Serialize};

/// An opaque, backend-independent identity for a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceId(String);

impl WorkspaceId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An opaque identity for a build component, such as a Gradle source set or a
/// future Cargo package. It must not encode Gradle-specific semantics.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ComponentId(String);

impl ComponentId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An opaque identity for one indexed source unit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceUnitId(String);

impl SourceUnitId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An opaque identity for a semantic symbol.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SymbolId(String);

impl SymbolId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An opaque identity for a normalized type record.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TypeId(String);

impl TypeId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A slash-separated path relative to the workspace root.
///
/// Absolute paths, `.` segments, and paths escaping through `..` are rejected
/// at the frontend/importer boundary; this type records the normalized result.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspacePath(String);

impl WorkspacePath {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A hash/fingerprint encoded as `<algorithm>:<lowercase-hex>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Fingerprint(String);

impl Fingerprint {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Language selected for a source unit or fact set.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Kotlin,
    Java,
    Rust,
    TypeScript,
    Other(String),
}

/// Input system that produced a component graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildSystem {
    Gradle,
    Maven,
    Cargo,
    Npm,
    Bazel,
    Filesystem,
    Other(String),
}

/// Whether a source unit is user-maintained, generated, or binary-derived.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceOrigin {
    Source,
    Generated,
    Dependency,
}

/// The lifecycle state of a persisted fact set relative to current inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    Fresh,
    Stale,
    Unknown,
    Unsupported,
}

/// Whether a backend produced all facts requested for a source unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Completeness {
    Complete,
    Partial,
    Failed,
}

/// How trustworthy a returned relation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Precision {
    Exact,
    Approximate,
}

/// Identifies the worker implementation that supplied a fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub backend: String,
    pub backend_version: String,
    pub protocol_version: u32,
    pub analysis_options: Fingerprint,
}

/// Position supplied to the CLI/API. Lines and columns are one-based Unicode
/// scalar-value counts. A position is a cursor location, not a byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TextPosition {
    pub line: u32,
    pub column: u32,
}

/// Half-open UTF-8 byte range in the exact content snapshot of a source unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

/// A user-facing position together with a workspace-relative source path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Location {
    pub path: WorkspacePath,
    pub position: TextPosition,
}

/// A source range attributable to one unit and snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceRange {
    pub source_unit: SourceUnitId,
    pub bytes: ByteRange,
}

/// A component discovered from a project model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component {
    pub id: ComponentId,
    pub name: String,
    pub build_system: BuildSystem,
    pub root: WorkspacePath,
    pub languages: Vec<Language>,
    pub configuration: Fingerprint,
}

/// A relation from one component to another component or immutable artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target_kind", rename_all = "snake_case")]
pub enum DependencyTarget {
    Component { component: ComponentId },
    Artifact { content: Fingerprint },
}

/// A canonical project model produced by a disposable build-system worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectManifest {
    pub workspace: WorkspaceId,
    pub root: WorkspacePath,
    pub components: Vec<Component>,
    pub dependencies: Vec<DependencyEdge>,
    pub fingerprint: Fingerprint,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyEdge {
    pub from: ComponentId,
    pub target: DependencyTarget,
    pub scope: String,
}

/// A single file or binary-derived source unit in one component context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceUnit {
    pub id: SourceUnitId,
    pub component: ComponentId,
    pub path: WorkspacePath,
    pub language: Language,
    pub origin: SourceOrigin,
    pub content: Fingerprint,
    pub context: Fingerprint,
}

/// A language backend's stable key. KIDE treats `value` as opaque and only
/// compares it under the same backend and key-schema version.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BackendKey {
    pub backend: String,
    pub schema_version: u32,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Package,
    Module,
    Class,
    Interface,
    Object,
    Enum,
    Function,
    Method,
    Constructor,
    Property,
    Field,
    Parameter,
    TypeAlias,
    Other,
}

/// The canonical declaration record persisted by KIDE.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolRecord {
    pub id: SymbolId,
    pub backend_key: BackendKey,
    pub language: Language,
    pub kind: SymbolKind,
    pub name: String,
    pub qualified_name: Option<String>,
    pub signature: Option<String>,
    pub component: ComponentId,
    pub declaration: SourceRange,
    pub name_range: SourceRange,
    pub owner: Option<SymbolId>,
    pub freshness: Freshness,
    pub completeness: Completeness,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OccurrenceKind {
    Declaration,
    Reference,
    Call,
    TypeReference,
    Import,
}

/// An interval entry for resolving a user location to a symbol or relation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceOccurrence {
    pub range: SourceRange,
    pub kind: OccurrenceKind,
    pub enclosing_symbol: Option<SymbolId>,
    pub target: Option<SymbolId>,
    pub type_id: Option<TypeId>,
    pub precision: Precision,
    pub freshness: Freshness,
    pub completeness: Completeness,
    pub provenance: Provenance,
}

/// A semantic use of a resolved declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceEdge {
    pub source: SourceOccurrence,
    pub target: SymbolId,
    pub precision: Precision,
}

/// A call site and the exact callable selected in its component context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallEdge {
    pub source: SourceOccurrence,
    pub target: SymbolId,
    pub caller: Option<SymbolId>,
    pub precision: Precision,
}

/// An exact subtype/supertype or implementation relation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HierarchyEdge {
    pub subtype: SymbolId,
    pub supertype: SymbolId,
    pub precision: Precision,
    pub provenance: Provenance,
}

/// A backend-normalized type fact. The display form is intentionally language
/// specific; the ID and provenance identify its semantic source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeRecord {
    pub id: TypeId,
    pub language: Language,
    pub display: String,
    pub backend_key: Option<BackendKey>,
    pub freshness: Freshness,
    pub completeness: Completeness,
    pub provenance: Provenance,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_id_preserves_its_opaque_value() {
        let id = SymbolId::new("kotlin:example.Service#run()V");

        assert_eq!(id.as_str(), "kotlin:example.Service#run()V");
    }

    #[test]
    fn a_non_jvm_language_fits_the_canonical_source_unit() {
        let source = SourceUnit {
            id: SourceUnitId::new("typescript:ui:src/main.ts"),
            component: ComponentId::new("npm:ui"),
            path: WorkspacePath::new("ui/src/main.ts"),
            language: Language::TypeScript,
            origin: SourceOrigin::Source,
            content: Fingerprint::new("sha256:abc"),
            context: Fingerprint::new("sha256:def"),
        };

        assert_eq!(source.language, Language::TypeScript);
        assert_eq!(source.component.as_str(), "npm:ui");
    }
}
