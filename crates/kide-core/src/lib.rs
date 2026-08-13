//! Persistent, frontend-independent KIDE primitives.
//!
//! This crate will own canonical records and query interfaces. Language and
//! build-system workers remain outside this crate and are disposable compute
//! processes as defined by ADR 0001.

use serde::Serialize;

/// Index format owned by KIDE Core, independent of any worker's internal AST.
pub const INDEX_FORMAT_VERSION: u32 = 1;

/// A lightweight identity used until the canonical symbol schema is introduced.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct SymbolId(String);

impl SymbolId {
    /// Creates an opaque symbol identifier.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the serialized form used by frontends and worker payloads.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_id_preserves_its_opaque_value() {
        let id = SymbolId::new("kotlin:example.Service#run()V");

        assert_eq!(id.as_str(), "kotlin:example.Service#run()V");
    }
}
