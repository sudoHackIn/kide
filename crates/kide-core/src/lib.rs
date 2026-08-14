//! Persistent, frontend-independent KIDE primitives.
//!
//! The canonical model intentionally persists graph facts, not a universal AST
//! or compiler object graph. Language and build-system workers remain
//! disposable compute processes as defined by ADR 0001.

mod canonical;
mod discovery;
mod freshness;
mod orchestrator;
mod protocol;
mod query;
mod store;
mod supervisor;

pub use canonical::*;
pub use discovery::*;
pub use freshness::*;
pub use orchestrator::*;
pub use protocol::*;
pub use query::*;
pub use store::*;
pub use supervisor::*;

/// Version of the normalized records and JSON envelopes owned by KIDE Core.
pub const CANONICAL_SCHEMA_VERSION: u32 = 1;

/// Format of the physical persistent index.
///
/// The storage engine may evolve independently, but a reader must reject a
/// newer incompatible format rather than treating it as fresh data.
pub const INDEX_FORMAT_VERSION: u32 = 1;
