//! SQLite persistence for canonical KIDE facts.
//!
//! A worker never receives this connection. Core validates a complete file
//! snapshot and replaces its rows in one transaction, so a crash can leave
//! either the old snapshot or the new one, never a mixture.

use std::{
    fs,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use thiserror::Error;

use crate::{
    AnalysisInput, ByteRange, CallEdge, ComponentId, DiagnosticRecord, FileAnalysisSnapshot,
    HierarchyEdge, INDEX_FORMAT_VERSION, ProjectManifest, Provenance, ReferenceEdge,
    SourceOccurrence, SourceUnit, SourceUnitId, SymbolId, SymbolRecord, TypeRecord,
    WORKER_PROTOCOL_VERSION, WorkspacePath,
};

const MIGRATION_1: &str = r#"
CREATE TABLE IF NOT EXISTS kide_metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE IF NOT EXISTS project_manifests (
    workspace_id TEXT PRIMARY KEY,
    fingerprint TEXT NOT NULL,
    protocol_version INTEGER NOT NULL,
    record_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS source_snapshots (
    source_unit_id TEXT PRIMARY KEY,
    component_id TEXT NOT NULL,
    workspace_path TEXT NOT NULL,
    content_fingerprint TEXT NOT NULL,
    context_fingerprint TEXT NOT NULL,
    protocol_version INTEGER NOT NULL,
    source_unit_json TEXT NOT NULL,
    snapshot_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS source_snapshots_by_path
    ON source_snapshots(workspace_path, source_unit_id);
CREATE TABLE IF NOT EXISTS symbols (
    symbol_id TEXT PRIMARY KEY,
    source_unit_id TEXT NOT NULL,
    name TEXT NOT NULL,
    name_start_byte INTEGER NOT NULL,
    record_blob BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS symbols_by_name
    ON symbols(name, source_unit_id, name_start_byte, symbol_id);
CREATE TABLE IF NOT EXISTS occurrences (
    source_unit_id TEXT NOT NULL,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    occurrence_kind TEXT NOT NULL,
    target_symbol_id TEXT,
    record_json TEXT NOT NULL,
    PRIMARY KEY (source_unit_id, start_byte, end_byte, occurrence_kind)
);
CREATE INDEX IF NOT EXISTS occurrences_by_source_interval
    ON occurrences(source_unit_id, start_byte, end_byte);
CREATE TABLE IF NOT EXISTS reference_edges (
    source_unit_id TEXT NOT NULL,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    target_symbol_id TEXT NOT NULL,
    record_json TEXT NOT NULL,
    PRIMARY KEY (source_unit_id, start_byte, end_byte, target_symbol_id)
);
CREATE INDEX IF NOT EXISTS references_by_target
    ON reference_edges(target_symbol_id, source_unit_id, start_byte);
CREATE TABLE IF NOT EXISTS call_edges (
    source_unit_id TEXT NOT NULL,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    target_symbol_id TEXT NOT NULL,
    record_json TEXT NOT NULL,
    PRIMARY KEY (source_unit_id, start_byte, end_byte, target_symbol_id)
);
CREATE INDEX IF NOT EXISTS calls_by_target
    ON call_edges(target_symbol_id, source_unit_id, start_byte);
CREATE TABLE IF NOT EXISTS hierarchy_edges (
    source_unit_id TEXT NOT NULL,
    subtype_symbol_id TEXT NOT NULL,
    supertype_symbol_id TEXT NOT NULL,
    record_json TEXT NOT NULL,
    PRIMARY KEY (source_unit_id, subtype_symbol_id, supertype_symbol_id)
);
CREATE INDEX IF NOT EXISTS hierarchy_by_supertype
    ON hierarchy_edges(supertype_symbol_id, source_unit_id, subtype_symbol_id);
CREATE TABLE IF NOT EXISTS type_records (
    source_unit_id TEXT NOT NULL,
    type_id TEXT NOT NULL,
    record_json TEXT NOT NULL,
    PRIMARY KEY (source_unit_id, type_id)
);
CREATE TABLE IF NOT EXISTS diagnostics (
    source_unit_id TEXT NOT NULL,
    start_byte INTEGER,
    end_byte INTEGER,
    severity TEXT NOT NULL,
    record_json TEXT NOT NULL,
    PRIMARY KEY (source_unit_id, start_byte, end_byte, severity, record_json)
);
"#;

const MIGRATION_2: &str = r#"
ALTER TABLE source_snapshots ADD COLUMN provenance_blob BLOB;
ALTER TABLE source_snapshots DROP COLUMN snapshot_json;
"#;

const SYMBOL_RECORD_FORMAT_VERSION: u8 = 1;

/// Failures that Core can surface without treating a partially written index as
/// a valid one.
#[derive(Debug, Error)]
pub enum IndexStoreError {
    #[error("index I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("canonical record JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("compact canonical record failed: {0}")]
    Binary(#[from] Box<bincode::ErrorKind>),
    #[error("symbol record binary format {found} is incompatible")]
    IncompatibleSymbolRecordFormat { found: u8 },
    #[error("index format {found} is incompatible; supported format is {supported}")]
    IncompatibleIndexFormat { found: u32, supported: u32 },
    #[error("worker protocol {found} is incompatible; supported protocol is {supported}")]
    IncompatibleWorkerProtocol { found: u32, supported: u32 },
    #[error("snapshot source unit does not match the source unit Core requested")]
    SourceUnitMismatch,
    #[error("snapshot content or context fingerprint changed before commit")]
    SnapshotInputMismatch,
    #[error("{record} belongs to {actual}, not snapshot source unit {expected}")]
    FactOwnedByOtherSource {
        record: &'static str,
        expected: String,
        actual: String,
    },
    #[error("{record} has an invalid byte range {start}..{end}")]
    InvalidRange {
        record: &'static str,
        start: u64,
        end: u64,
    },
    #[error("byte offset {value} cannot be represented by SQLite")]
    ByteOffsetOutOfRange { value: u64 },
}

/// A durable local index. The type is deliberately not `Clone`: one Core
/// process owns write serialization while SQLite WAL keeps readers available.
pub struct IndexStore {
    connection: Connection,
}

impl IndexStore {
    /// Standard per-workspace location selected in ADR 0002.
    pub fn default_path(workspace_root: &Path) -> PathBuf {
        workspace_root.join(".kide/index-v4.sqlite3")
    }

    /// Opens (and, on first use, creates) the current SQLite index format.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, IndexStoreError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let connection = Connection::open(path)?;
        let journal_mode: String =
            connection.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        debug_assert_eq!(journal_mode, "wal");
        connection.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;",
        )?;

        let store = Self { connection };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), IndexStoreError> {
        self.connection.execute_batch(MIGRATION_1)?;
        let stored_version: Option<String> = self
            .connection
            .query_row(
                "SELECT value FROM kide_metadata WHERE key = 'index_format_version'",
                [],
                |row| row.get(0),
            )
            .optional()?;

        match stored_version {
            Some(value) => {
                let found = value.parse::<u32>().unwrap_or(u32::MAX);
                if found != INDEX_FORMAT_VERSION {
                    return Err(IndexStoreError::IncompatibleIndexFormat {
                        found,
                        supported: INDEX_FORMAT_VERSION,
                    });
                }
            }
            None => {
                self.connection.execute(
                    "INSERT INTO kide_metadata (key, value) VALUES ('index_format_version', ?1)",
                    params![INDEX_FORMAT_VERSION.to_string()],
                )?;
            }
        }
        self.connection.execute(
            "INSERT OR IGNORE INTO schema_migrations (version) VALUES (1)",
            [],
        )?;
        let migration_2: Option<u32> = self
            .connection
            .query_row(
                "SELECT version FROM schema_migrations WHERE version = 2",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if migration_2.is_none() {
            self.connection.execute_batch(MIGRATION_2)?;
            self.connection
                .execute("INSERT INTO schema_migrations (version) VALUES (2)", [])?;
        }
        Ok(())
    }

    pub fn index_format_version(&self) -> u32 {
        INDEX_FORMAT_VERSION
    }

    /// Persists a build manifest independently from live worker state.
    pub fn put_manifest(&self, manifest: &ProjectManifest) -> Result<(), IndexStoreError> {
        validate_provenance(&manifest.provenance)?;
        self.connection.execute(
            "INSERT INTO project_manifests (workspace_id, fingerprint, protocol_version, record_json)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(workspace_id) DO UPDATE SET
               fingerprint=excluded.fingerprint,
               protocol_version=excluded.protocol_version,
               record_json=excluded.record_json",
            params![
                manifest.workspace.as_str(),
                manifest.fingerprint.as_str(),
                manifest.provenance.protocol_version,
                serde_json::to_string(manifest)?,
            ],
        )?;
        Ok(())
    }

    pub fn manifest(
        &self,
        workspace: &crate::WorkspaceId,
    ) -> Result<Option<ProjectManifest>, IndexStoreError> {
        self.json_optional(
            "SELECT record_json FROM project_manifests WHERE workspace_id = ?1",
            workspace.as_str(),
        )
    }

    /// Atomically replaces every fact owned by one source unit. `expected`
    /// comes from discovery immediately before the worker was launched.
    pub fn replace_snapshot(
        &mut self,
        expected: &SourceUnit,
        snapshot: &FileAnalysisSnapshot,
    ) -> Result<(), IndexStoreError> {
        validate_snapshot(expected, snapshot)?;
        let _span = tracing::debug_span!(target: "kide::store", "replace_snapshot", source_unit_id = %expected.id.as_str()).entered();
        tracing::debug!(target: "kide::store", symbols = snapshot.symbols.len(), "writing snapshot");
        let transaction = self.connection.transaction()?;
        delete_file_owned_facts(&transaction, &expected.id)?;
        insert_snapshot(&transaction, snapshot)?;
        transaction.commit()?;
        tracing::debug!(target: "kide::store", "snapshot committed");
        Ok(())
    }

    pub fn source_unit(&self, id: &SourceUnitId) -> Result<Option<SourceUnit>, IndexStoreError> {
        self.json_optional(
            "SELECT source_unit_json FROM source_snapshots WHERE source_unit_id = ?1",
            id.as_str(),
        )
    }

    /// Returns every persisted source input in stable order for incremental planning.
    pub fn source_units(&self) -> Result<Vec<SourceUnit>, IndexStoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT source_unit_json FROM source_snapshots ORDER BY source_unit_id")?;
        let records = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        records
            .into_iter()
            .map(|record| serde_json::from_str(&record).map_err(IndexStoreError::from))
            .collect()
    }

    /// Finds persisted units at one workspace-relative path. Multiple build
    /// components may legitimately contribute the same generated path.
    pub fn source_units_at_path(
        &self,
        path: &WorkspacePath,
    ) -> Result<Vec<SourceUnit>, IndexStoreError> {
        let mut statement = self.connection.prepare(
            "SELECT source_unit_json FROM source_snapshots
             WHERE workspace_path = ?1 ORDER BY source_unit_id",
        )?;
        let records = statement
            .query_map(params![path.as_str()], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        records
            .into_iter()
            .map(|record| serde_json::from_str(&record).map_err(IndexStoreError::from))
            .collect()
    }

    /// Returns persisted source inputs together with the worker provenance
    /// that produced them, for conservative incremental planning.
    pub fn analysis_inputs(&self) -> Result<Vec<AnalysisInput>, IndexStoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT source_unit_json, provenance_blob FROM source_snapshots ORDER BY source_unit_id")?;
        let snapshots = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        snapshots
            .into_iter()
            .map(|(source, provenance)| {
                let source_unit = serde_json::from_str(&source)?;
                let provenance = bincode::deserialize(&provenance)?;
                Ok(AnalysisInput {
                    source_unit,
                    provenance,
                })
            })
            .collect()
    }

    /// Removes all facts owned by a source unit in one transaction.
    pub fn remove_snapshot(&mut self, source_unit: &SourceUnitId) -> Result<(), IndexStoreError> {
        let transaction = self.connection.transaction()?;
        delete_file_owned_facts(&transaction, source_unit)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn symbols_named(&self, name: &str) -> Result<Vec<SymbolRecord>, IndexStoreError> {
        let mut statement = self.connection.prepare(
            "SELECT symbols.symbol_id, symbols.name, symbols.record_blob, source_snapshots.source_unit_json, source_snapshots.provenance_blob
             FROM symbols JOIN source_snapshots USING (source_unit_id)
             WHERE symbols.name = ?1
             ORDER BY symbols.source_unit_id, symbols.name_start_byte, symbols.symbol_id",
        )?;
        let rows = statement
            .query_map(params![name], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, name, record, source, provenance)| {
                let record = decode_stored_symbol(&record)?;
                let source_unit = serde_json::from_str(&source)?;
                let provenance = bincode::deserialize(&provenance)?;
                Ok(record.into_symbol(SymbolId::new(id), name, &source_unit, &provenance))
            })
            .collect()
    }

    /// Decodes declarations owned by one source unit in source order. This is
    /// used only for a location resolver after the path has narrowed the
    /// candidate set to one unit.
    pub fn symbols_for_source(
        &self,
        source_unit: &SourceUnitId,
    ) -> Result<Vec<SymbolRecord>, IndexStoreError> {
        let mut statement = self.connection.prepare(
            "SELECT symbols.symbol_id, symbols.name, symbols.record_blob, source_snapshots.source_unit_json, source_snapshots.provenance_blob
             FROM symbols JOIN source_snapshots USING (source_unit_id)
             WHERE symbols.source_unit_id = ?1
             ORDER BY symbols.name_start_byte, symbols.symbol_id",
        )?;
        let rows = statement
            .query_map(params![source_unit.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, name, record, source, provenance)| {
                let record = decode_stored_symbol(&record)?;
                let source_unit = serde_json::from_str(&source)?;
                let provenance = bincode::deserialize(&provenance)?;
                Ok(record.into_symbol(SymbolId::new(id), name, &source_unit, &provenance))
            })
            .collect()
    }

    /// Looks up one declaration by its stable semantic ID.
    pub fn symbol(&self, symbol: &SymbolId) -> Result<Option<SymbolRecord>, IndexStoreError> {
        let mut statement = self.connection.prepare(
            "SELECT symbols.symbol_id, symbols.name, symbols.record_blob, source_snapshots.source_unit_json, source_snapshots.provenance_blob
             FROM symbols JOIN source_snapshots USING (source_unit_id)
             WHERE symbols.symbol_id = ?1",
        )?;
        statement
            .query_row(params![symbol.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            })
            .optional()?
            .map(|(id, name, record, source, provenance)| {
                let record = decode_stored_symbol(&record)?;
                let source_unit = serde_json::from_str(&source)?;
                let provenance = bincode::deserialize(&provenance)?;
                Ok(record.into_symbol(SymbolId::new(id), name, &source_unit, &provenance))
            })
            .transpose()
    }

    pub fn occurrences_at(
        &self,
        source_unit: &SourceUnitId,
        byte_offset: u64,
    ) -> Result<Vec<SourceOccurrence>, IndexStoreError> {
        self.json_many_with_i64(
            "SELECT record_json FROM occurrences
             WHERE source_unit_id = ?1 AND start_byte <= ?2 AND end_byte > ?2
             ORDER BY start_byte DESC, end_byte ASC",
            source_unit.as_str(),
            sqlite_offset(byte_offset)?,
        )
    }

    pub fn references_to(&self, symbol: &SymbolId) -> Result<Vec<ReferenceEdge>, IndexStoreError> {
        self.json_many(
            "SELECT record_json FROM reference_edges WHERE target_symbol_id = ?1
             ORDER BY source_unit_id, start_byte",
            symbol.as_str(),
        )
    }

    pub fn calls_to(&self, symbol: &SymbolId) -> Result<Vec<CallEdge>, IndexStoreError> {
        self.json_many(
            "SELECT record_json FROM call_edges WHERE target_symbol_id = ?1
             ORDER BY source_unit_id, start_byte",
            symbol.as_str(),
        )
    }

    pub fn implementations_of(
        &self,
        symbol: &SymbolId,
    ) -> Result<Vec<HierarchyEdge>, IndexStoreError> {
        self.json_many(
            "SELECT record_json FROM hierarchy_edges WHERE supertype_symbol_id = ?1
             ORDER BY source_unit_id, subtype_symbol_id",
            symbol.as_str(),
        )
    }

    pub fn types_for(
        &self,
        source_unit: &SourceUnitId,
    ) -> Result<Vec<TypeRecord>, IndexStoreError> {
        self.json_many(
            "SELECT record_json FROM type_records WHERE source_unit_id = ?1 ORDER BY type_id",
            source_unit.as_str(),
        )
    }

    pub fn diagnostics_for(
        &self,
        source_unit: &SourceUnitId,
    ) -> Result<Vec<DiagnosticRecord>, IndexStoreError> {
        self.json_many(
            "SELECT record_json FROM diagnostics WHERE source_unit_id = ?1
             ORDER BY start_byte, end_byte, severity",
            source_unit.as_str(),
        )
    }

    fn json_optional<T: serde::de::DeserializeOwned>(
        &self,
        sql: &str,
        value: &str,
    ) -> Result<Option<T>, IndexStoreError> {
        let json: Option<String> = self
            .connection
            .query_row(sql, params![value], |row| row.get(0))
            .optional()?;
        json.map(|record| serde_json::from_str(&record).map_err(IndexStoreError::from))
            .transpose()
    }

    fn json_many<T: serde::de::DeserializeOwned>(
        &self,
        sql: &str,
        value: &str,
    ) -> Result<Vec<T>, IndexStoreError> {
        let mut statement = self.connection.prepare(sql)?;
        let records = statement
            .query_map(params![value], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        records
            .into_iter()
            .map(|record| serde_json::from_str(&record).map_err(IndexStoreError::from))
            .collect()
    }

    fn json_many_with_i64<T: serde::de::DeserializeOwned>(
        &self,
        sql: &str,
        value: &str,
        offset: i64,
    ) -> Result<Vec<T>, IndexStoreError> {
        let mut statement = self.connection.prepare(sql)?;
        let records = statement
            .query_map(params![value, offset], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        records
            .into_iter()
            .map(|record| serde_json::from_str(&record).map_err(IndexStoreError::from))
            .collect()
    }
}

fn validate_snapshot(
    expected: &SourceUnit,
    snapshot: &FileAnalysisSnapshot,
) -> Result<(), IndexStoreError> {
    if snapshot.source_unit.id != expected.id {
        return Err(IndexStoreError::SourceUnitMismatch);
    }
    if snapshot.source_unit.content != expected.content
        || snapshot.source_unit.context != expected.context
    {
        return Err(IndexStoreError::SnapshotInputMismatch);
    }
    validate_provenance(&snapshot.provenance)?;
    let source_id = expected.id.as_str();

    for symbol in &snapshot.symbols {
        validate_provenance(&symbol.provenance)?;
        ensure_fact_source(
            "symbol declaration",
            source_id,
            &symbol.declaration.source_unit,
        )?;
        ensure_fact_source(
            "symbol name range",
            source_id,
            &symbol.name_range.source_unit,
        )?;
        validate_range("symbol declaration", &symbol.declaration.bytes)?;
        validate_range("symbol name range", &symbol.name_range.bytes)?;
    }
    for occurrence in &snapshot.occurrences {
        validate_occurrence(source_id, occurrence)?;
    }
    for reference in &snapshot.references {
        validate_occurrence(source_id, &reference.source)?;
    }
    for call in &snapshot.calls {
        validate_occurrence(source_id, &call.source)?;
    }
    for edge in &snapshot.hierarchy {
        validate_provenance(&edge.provenance)?;
    }
    for ty in &snapshot.types {
        validate_provenance(&ty.provenance)?;
    }
    for diagnostic in &snapshot.diagnostics {
        validate_provenance(&diagnostic.provenance)?;
        ensure_fact_source("diagnostic", source_id, &diagnostic.source_unit)?;
        if let Some(range) = &diagnostic.range {
            validate_range("diagnostic", range)?;
        }
    }
    Ok(())
}

fn validate_occurrence(
    source_id: &str,
    occurrence: &SourceOccurrence,
) -> Result<(), IndexStoreError> {
    validate_provenance(&occurrence.provenance)?;
    ensure_fact_source("occurrence", source_id, &occurrence.range.source_unit)?;
    validate_range("occurrence", &occurrence.range.bytes)
}

fn validate_provenance(provenance: &Provenance) -> Result<(), IndexStoreError> {
    if provenance.protocol_version != WORKER_PROTOCOL_VERSION {
        return Err(IndexStoreError::IncompatibleWorkerProtocol {
            found: provenance.protocol_version,
            supported: WORKER_PROTOCOL_VERSION,
        });
    }
    Ok(())
}

fn ensure_fact_source(
    record: &'static str,
    expected: &str,
    actual: &SourceUnitId,
) -> Result<(), IndexStoreError> {
    if actual.as_str() == expected {
        Ok(())
    } else {
        Err(IndexStoreError::FactOwnedByOtherSource {
            record,
            expected: expected.to_owned(),
            actual: actual.as_str().to_owned(),
        })
    }
}

fn validate_range(record: &'static str, range: &crate::ByteRange) -> Result<(), IndexStoreError> {
    if range.start <= range.end {
        Ok(())
    } else {
        Err(IndexStoreError::InvalidRange {
            record,
            start: range.start,
            end: range.end,
        })
    }
}

fn sqlite_offset(value: u64) -> Result<i64, IndexStoreError> {
    i64::try_from(value).map_err(|_| IndexStoreError::ByteOffsetOutOfRange { value })
}

fn delete_file_owned_facts(
    transaction: &Transaction<'_>,
    source_unit: &SourceUnitId,
) -> Result<(), IndexStoreError> {
    for table in [
        "reference_edges",
        "call_edges",
        "hierarchy_edges",
        "type_records",
        "diagnostics",
        "occurrences",
        "symbols",
        "source_snapshots",
    ] {
        transaction.execute(
            &format!("DELETE FROM {table} WHERE source_unit_id = ?1"),
            params![source_unit.as_str()],
        )?;
    }
    Ok(())
}

fn insert_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &FileAnalysisSnapshot,
) -> Result<(), IndexStoreError> {
    let source = &snapshot.source_unit;
    transaction.execute(
        "INSERT INTO source_snapshots
         (source_unit_id, component_id, workspace_path, content_fingerprint, context_fingerprint,
          protocol_version, source_unit_json, provenance_blob)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            source.id.as_str(),
            source.component.as_str(),
            source.path.as_str(),
            source.content.as_str(),
            source.context.as_str(),
            snapshot.provenance.protocol_version,
            serde_json::to_string(source)?,
            bincode::serialize(&snapshot.provenance)?,
        ],
    )?;
    for symbol in &snapshot.symbols {
        transaction.execute(
            "INSERT INTO symbols (symbol_id, source_unit_id, name, name_start_byte, record_blob)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                symbol.id.as_str(),
                source.id.as_str(),
                symbol.name,
                sqlite_offset(symbol.name_range.bytes.start)?,
                encode_stored_symbol(&StoredSymbolRecord::from(symbol))?
            ],
        )?;
    }
    for occurrence in &snapshot.occurrences {
        transaction.execute(
            "INSERT INTO occurrences
             (source_unit_id, start_byte, end_byte, occurrence_kind, target_symbol_id, record_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                source.id.as_str(),
                sqlite_offset(occurrence.range.bytes.start)?,
                sqlite_offset(occurrence.range.bytes.end)?,
                format!("{:?}", occurrence.kind),
                occurrence.target.as_ref().map(SymbolId::as_str),
                serde_json::to_string(occurrence)?
            ],
        )?;
    }
    for reference in &snapshot.references {
        transaction.execute(
            "INSERT INTO reference_edges (source_unit_id, start_byte, end_byte, target_symbol_id, record_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![source.id.as_str(), sqlite_offset(reference.source.range.bytes.start)?, sqlite_offset(reference.source.range.bytes.end)?, reference.target.as_str(), serde_json::to_string(reference)?],
        )?;
    }
    for call in &snapshot.calls {
        transaction.execute(
            "INSERT INTO call_edges (source_unit_id, start_byte, end_byte, target_symbol_id, record_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![source.id.as_str(), sqlite_offset(call.source.range.bytes.start)?, sqlite_offset(call.source.range.bytes.end)?, call.target.as_str(), serde_json::to_string(call)?],
        )?;
    }
    for edge in &snapshot.hierarchy {
        transaction.execute(
            "INSERT INTO hierarchy_edges (source_unit_id, subtype_symbol_id, supertype_symbol_id, record_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![source.id.as_str(), edge.subtype.as_str(), edge.supertype.as_str(), serde_json::to_string(edge)?],
        )?;
    }
    for ty in &snapshot.types {
        transaction.execute(
            "INSERT INTO type_records (source_unit_id, type_id, record_json) VALUES (?1, ?2, ?3)",
            params![
                source.id.as_str(),
                ty.id.as_str(),
                serde_json::to_string(ty)?
            ],
        )?;
    }
    for diagnostic in &snapshot.diagnostics {
        let (start, end) = match diagnostic.range.as_ref() {
            Some(range) => (
                Some(sqlite_offset(range.start)?),
                Some(sqlite_offset(range.end)?),
            ),
            None => (None, None),
        };
        transaction.execute(
            "INSERT INTO diagnostics (source_unit_id, start_byte, end_byte, severity, record_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                source.id.as_str(),
                start,
                end,
                format!("{:?}", diagnostic.severity),
                serde_json::to_string(diagnostic)?
            ],
        )?;
    }
    Ok(())
}

/// The query columns own identity and lookup fields; the source snapshot owns
/// component and provenance. Do not repeat either in every dependency symbol.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct StoredSymbolRecord {
    backend_key: crate::BackendKey,
    language: crate::Language,
    kind: crate::SymbolKind,
    qualified_name: Option<String>,
    signature: Option<String>,
    declaration: ByteRange,
    name_range: ByteRange,
    owner: Option<SymbolId>,
    modifiers: Vec<String>,
    annotations: Vec<String>,
    freshness: crate::Freshness,
    completeness: crate::Completeness,
}

impl From<&SymbolRecord> for StoredSymbolRecord {
    fn from(symbol: &SymbolRecord) -> Self {
        Self {
            backend_key: symbol.backend_key.clone(),
            language: symbol.language.clone(),
            kind: symbol.kind,
            qualified_name: symbol.qualified_name.clone(),
            signature: symbol.signature.clone(),
            declaration: symbol.declaration.bytes,
            name_range: symbol.name_range.bytes,
            owner: symbol.owner.clone(),
            modifiers: symbol.modifiers.clone(),
            annotations: symbol.annotations.clone(),
            freshness: symbol.freshness,
            completeness: symbol.completeness,
        }
    }
}

impl StoredSymbolRecord {
    fn into_symbol(
        self,
        id: SymbolId,
        name: String,
        source_unit: &SourceUnit,
        provenance: &crate::Provenance,
    ) -> SymbolRecord {
        SymbolRecord {
            id,
            backend_key: self.backend_key,
            language: self.language,
            kind: self.kind,
            name,
            qualified_name: self.qualified_name,
            signature: self.signature,
            component: ComponentId::new(source_unit.component.as_str()),
            declaration: crate::SourceRange {
                source_unit: source_unit.id.clone(),
                bytes: self.declaration,
            },
            name_range: crate::SourceRange {
                source_unit: source_unit.id.clone(),
                bytes: self.name_range,
            },
            owner: self.owner,
            modifiers: self.modifiers,
            annotations: self.annotations,
            freshness: self.freshness,
            completeness: self.completeness,
            provenance: provenance.clone(),
        }
    }
}

fn encode_stored_symbol(record: &StoredSymbolRecord) -> Result<Vec<u8>, IndexStoreError> {
    let payload = bincode::serialize(record)?;
    let mut encoded = Vec::with_capacity(payload.len() + 1);
    encoded.push(SYMBOL_RECORD_FORMAT_VERSION);
    encoded.extend(payload);
    Ok(encoded)
}

fn decode_stored_symbol(encoded: &[u8]) -> Result<StoredSymbolRecord, IndexStoreError> {
    let (&version, payload) = encoded
        .split_first()
        .ok_or(IndexStoreError::IncompatibleSymbolRecordFormat { found: 0 })?;
    if version != SYMBOL_RECORD_FORMAT_VERSION {
        return Err(IndexStoreError::IncompatibleSymbolRecordFormat { found: version });
    }
    Ok(bincode::deserialize(payload)?)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::{
        BackendKey, ByteRange, CallEdge, Completeness, Component, ComponentId, DiagnosticSeverity,
        Fingerprint, Freshness, HierarchyEdge, Language, OccurrenceKind, Precision,
        ProjectManifest, Provenance, SourceOccurrence, SourceOrigin, SourceRange, SymbolKind,
        WorkspaceId, WorkspacePath,
    };

    use super::*;

    #[test]
    fn persists_and_recovers_every_file_owned_fact_after_restart() {
        let directory = tempdir().expect("temporary index directory");
        let path = directory.path().join("index.sqlite3");
        let source = source_unit("sha256:content-v1");
        let snapshot = snapshot(source.clone());
        let manifest = manifest();

        let mut store = IndexStore::open(&path).expect("opens index");
        store.put_manifest(&manifest).expect("stores manifest");
        store
            .replace_snapshot(&source, &snapshot)
            .expect("commits snapshot");
        drop(store);

        let store = IndexStore::open(&path).expect("reopens index");
        assert_eq!(
            store.manifest(&manifest.workspace).expect("reads manifest"),
            Some(manifest)
        );
        assert_eq!(
            store.source_unit(&source.id).expect("reads source"),
            Some(source.clone())
        );
        assert_eq!(
            store.symbols_named("PaymentService").expect("name lookup"),
            snapshot.symbols
        );
        assert_eq!(
            store
                .occurrences_at(&source.id, 24)
                .expect("interval lookup"),
            snapshot.occurrences
        );
        assert_eq!(
            store
                .references_to(&snapshot.symbols[0].id)
                .expect("reference lookup"),
            snapshot.references
        );
        assert_eq!(
            store
                .calls_to(&snapshot.symbols[0].id)
                .expect("call lookup"),
            snapshot.calls
        );
        assert_eq!(
            store
                .implementations_of(&snapshot.hierarchy[0].supertype)
                .expect("hierarchy lookup"),
            snapshot.hierarchy
        );
        assert_eq!(
            store.types_for(&source.id).expect("type lookup"),
            snapshot.types
        );
        assert_eq!(
            store
                .diagnostics_for(&source.id)
                .expect("diagnostics lookup"),
            snapshot.diagnostics
        );
    }

    #[test]
    fn rejected_snapshot_keeps_the_previous_committed_snapshot() {
        let directory = tempdir().expect("temporary index directory");
        let mut store =
            IndexStore::open(directory.path().join("index.sqlite3")).expect("opens index");
        let old_source = source_unit("sha256:content-v1");
        let old_snapshot = snapshot(old_source.clone());
        store
            .replace_snapshot(&old_source, &old_snapshot)
            .expect("commits initial snapshot");

        let expected_next = source_unit("sha256:content-v2");
        let error = store
            .replace_snapshot(&expected_next, &old_snapshot)
            .expect_err("content mismatch is rejected");
        assert!(matches!(error, IndexStoreError::SnapshotInputMismatch));
        assert_eq!(
            store
                .source_unit(&old_source.id)
                .expect("reads existing source"),
            Some(old_source)
        );
        assert_eq!(
            store
                .symbols_named("PaymentService")
                .expect("reads existing facts"),
            old_snapshot.symbols
        );
    }

    #[test]
    fn lists_inputs_and_removes_a_deleted_source_atomically() {
        let directory = tempdir().expect("temporary index directory");
        let mut store =
            IndexStore::open(directory.path().join("index.sqlite3")).expect("opens index");
        let source = source_unit("sha256:content-v1");
        store
            .replace_snapshot(&source, &snapshot(source.clone()))
            .expect("commits snapshot");

        assert_eq!(
            store.source_units().expect("lists inputs"),
            vec![source.clone()]
        );
        assert_eq!(
            store.analysis_inputs().expect("lists analysis inputs"),
            vec![AnalysisInput {
                source_unit: source.clone(),
                provenance: provenance(),
            }]
        );
        store.remove_snapshot(&source.id).expect("removes source");
        assert!(store.source_units().expect("lists inputs").is_empty());
        assert!(
            store
                .symbols_named("PaymentService")
                .expect("reads facts")
                .is_empty()
        );
    }

    #[test]
    fn format_version_mismatch_is_explicit() {
        let directory = tempdir().expect("temporary index directory");
        let path = directory.path().join("index.sqlite3");
        drop(IndexStore::open(&path).expect("creates index"));
        let connection = Connection::open(&path).expect("opens raw index");
        connection
            .execute(
                "UPDATE kide_metadata SET value = '5' WHERE key = 'index_format_version'",
                [],
            )
            .expect("changes version");
        drop(connection);

        let error = match IndexStore::open(&path) {
            Ok(_) => panic!("future index is rejected"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            IndexStoreError::IncompatibleIndexFormat {
                found: 5,
                supported: 4
            }
        ));
    }

    #[test]
    fn snapshot_with_incompatible_protocol_is_rejected_before_commit() {
        let directory = tempdir().expect("temporary index directory");
        let mut store =
            IndexStore::open(directory.path().join("index.sqlite3")).expect("opens index");
        let source = source_unit("sha256:content-v1");
        let mut incompatible = snapshot(source.clone());
        incompatible.provenance.protocol_version = WORKER_PROTOCOL_VERSION + 1;

        let error = store
            .replace_snapshot(&source, &incompatible)
            .expect_err("future protocol is rejected");
        assert!(matches!(
            error,
            IndexStoreError::IncompatibleWorkerProtocol { found, supported }
                if found == WORKER_PROTOCOL_VERSION + 1 && supported == WORKER_PROTOCOL_VERSION
        ));
        assert_eq!(store.source_unit(&source.id).expect("reads source"), None);
    }

    #[test]
    fn dependency_facts_remain_queryable_without_a_full_snapshot_payload() {
        let directory = tempdir().expect("temporary index directory");
        let mut store =
            IndexStore::open(directory.path().join("index.sqlite3")).expect("opens index");
        let mut dependency = source_unit("sha256:artifact-v1");
        dependency.origin = SourceOrigin::Dependency;
        dependency.id = SourceUnitId::new("jvm:sha256:artifact-v1");
        let full = snapshot(dependency.clone());

        store
            .replace_snapshot(&dependency, &full)
            .expect("stores dependency facts");

        assert_eq!(
            store
                .symbols_named("PaymentService")
                .expect("queries symbol"),
            full.symbols
        );
        assert_eq!(
            store.analysis_inputs().expect("keeps planning input"),
            vec![AnalysisInput {
                source_unit: dependency.clone(),
                provenance: full.provenance.clone(),
            }]
        );
        let provenance: Vec<u8> = store
            .connection
            .query_row(
                "SELECT provenance_blob FROM source_snapshots WHERE source_unit_id = ?1",
                params![dependency.id.as_str()],
                |row| row.get(0),
            )
            .expect("reads compact provenance");
        assert_eq!(
            bincode::deserialize::<Provenance>(&provenance).expect("decodes provenance"),
            full.provenance
        );
    }

    #[test]
    fn compact_symbol_payload_is_binary_versioned_and_smaller_than_json() {
        let record =
            StoredSymbolRecord::from(&snapshot(source_unit("sha256:content-v1")).symbols[0]);
        let encoded = encode_stored_symbol(&record).expect("encodes binary record");

        assert_eq!(
            decode_stored_symbol(&encoded).expect("decodes record"),
            record
        );
        assert!(encoded.len() < serde_json::to_vec(&record).expect("encodes JSON").len());
        assert!(matches!(
            decode_stored_symbol(&[SYMBOL_RECORD_FORMAT_VERSION + 1]),
            Err(IndexStoreError::IncompatibleSymbolRecordFormat { .. })
        ));
    }

    fn source_unit(content: &str) -> SourceUnit {
        SourceUnit {
            id: SourceUnitId::new("gradle:app:main:PaymentService.kt"),
            component: ComponentId::new("gradle:app:main"),
            path: WorkspacePath::new("src/main/kotlin/PaymentService.kt"),
            language: Language::Kotlin,
            origin: SourceOrigin::Source,
            content: Fingerprint::new(content),
            context: Fingerprint::new("sha256:context-v1"),
        }
    }

    fn snapshot(source_unit: SourceUnit) -> FileAnalysisSnapshot {
        let provenance = provenance();
        let symbol = SymbolRecord {
            id: SymbolId::new("kotlin:demo.PaymentService"),
            backend_key: BackendKey {
                backend: "kide-kotlin-jvm".to_owned(),
                schema_version: 1,
                value: "PaymentService".to_owned(),
            },
            language: Language::Kotlin,
            kind: SymbolKind::Class,
            name: "PaymentService".to_owned(),
            qualified_name: Some("demo.PaymentService".to_owned()),
            signature: None,
            component: source_unit.component.clone(),
            declaration: SourceRange {
                source_unit: source_unit.id.clone(),
                bytes: ByteRange { start: 6, end: 26 },
            },
            name_range: SourceRange {
                source_unit: source_unit.id.clone(),
                bytes: ByteRange { start: 12, end: 26 },
            },
            owner: None,
            modifiers: Vec::new(),
            annotations: Vec::new(),
            freshness: Freshness::Fresh,
            completeness: Completeness::Complete,
            provenance: provenance.clone(),
        };
        let occurrence = SourceOccurrence {
            range: SourceRange {
                source_unit: source_unit.id.clone(),
                bytes: ByteRange { start: 20, end: 25 },
            },
            kind: OccurrenceKind::Reference,
            enclosing_symbol: None,
            target: Some(symbol.id.clone()),
            type_id: Some(crate::TypeId::new("kotlin:demo.PaymentService")),
            precision: Precision::Exact,
            freshness: Freshness::Fresh,
            completeness: Completeness::Complete,
            provenance: provenance.clone(),
        };
        let supertype = SymbolId::new("kotlin:demo.PaymentProvider");
        FileAnalysisSnapshot {
            source_unit: source_unit.clone(),
            structural_fingerprint: None,
            public_api_fingerprint: None,
            symbols: vec![symbol.clone()],
            occurrences: vec![occurrence.clone()],
            references: vec![ReferenceEdge {
                source: occurrence.clone(),
                target: symbol.id.clone(),
                precision: Precision::Exact,
            }],
            calls: vec![CallEdge {
                source: occurrence,
                target: symbol.id.clone(),
                caller: None,
                precision: Precision::Exact,
            }],
            hierarchy: vec![HierarchyEdge {
                subtype: symbol.id.clone(),
                supertype,
                precision: Precision::Exact,
                provenance: provenance.clone(),
            }],
            types: vec![TypeRecord {
                id: crate::TypeId::new("kotlin:demo.PaymentService"),
                language: Language::Kotlin,
                display: "demo.PaymentService".to_owned(),
                backend_key: None,
                freshness: Freshness::Fresh,
                completeness: Completeness::Complete,
                provenance: provenance.clone(),
            }],
            diagnostics: vec![DiagnosticRecord {
                source_unit: source_unit.id.clone(),
                range: Some(ByteRange { start: 0, end: 5 }),
                severity: DiagnosticSeverity::Warning,
                code: Some("demo-warning".to_owned()),
                message: "demonstration diagnostic".to_owned(),
                freshness: Freshness::Fresh,
                completeness: Completeness::Complete,
                provenance: provenance.clone(),
            }],
            completeness: Completeness::Complete,
            provenance,
        }
    }

    fn manifest() -> ProjectManifest {
        ProjectManifest {
            workspace: WorkspaceId::new("workspace:demo"),
            root: WorkspacePath::new("."),
            components: vec![Component {
                id: ComponentId::new("gradle:app:main"),
                name: "app-main".to_owned(),
                build_system: crate::BuildSystem::Gradle,
                root: WorkspacePath::new("."),
                languages: vec![Language::Kotlin],
                configuration: Fingerprint::new("sha256:config"),
                source_sets: Vec::new(),
                classpath: Vec::new(),
                toolchain: None,
                compiler_configuration: None,
            }],
            dependencies: Vec::new(),
            fingerprint: Fingerprint::new("sha256:manifest"),
            provenance: provenance(),
        }
    }

    fn provenance() -> Provenance {
        Provenance {
            backend: "kide-kotlin-jvm".to_owned(),
            backend_version: "0.1.0".to_owned(),
            protocol_version: WORKER_PROTOCOL_VERSION,
            analysis_options: Fingerprint::new("sha256:default"),
        }
    }
}
