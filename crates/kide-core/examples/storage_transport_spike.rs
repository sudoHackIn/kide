//! Bounded storage and worker-transport proof for ADR 0002.
//!
//! Run with:
//! `cargo run -p kide-core --example storage_transport_spike`

use std::time::Instant;

use rusqlite::{Connection, Transaction, params};
use serde::{Deserialize, Serialize};
use tempfile::tempdir;

use kide_core::{
    BackendKey, ByteRange, CANONICAL_SCHEMA_VERSION, CallEdge, Completeness, ComponentId,
    Fingerprint, Freshness, Language, OccurrenceKind, Precision, Provenance, ReferenceEdge,
    SourceOccurrence, SourceRange, SourceUnitId, SymbolId, SymbolKind, SymbolRecord, TypeId,
};

const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AnalysisBatchProbe {
    protocol_version: u32,
    request_id: String,
    symbols: Vec<SymbolRecord>,
    occurrences: Vec<SourceOccurrence>,
    references: Vec<ReferenceEdge>,
    calls: Vec<CallEdge>,
}

#[derive(Debug, Serialize)]
struct ProbeMetrics {
    sqlite_version: String,
    sqlite_open_us: u128,
    snapshot_replace_us: u128,
    reopen_and_query_us: u128,
    ndjson_encode_us: u128,
    ndjson_decode_us: u128,
    ndjson_bytes: usize,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let database = temp.path().join("index.sqlite3");
    let source_unit = SourceUnitId::new("kotlin:app:src/main/kotlin/PaymentService.kt");
    let batch = probe_batch(&source_unit);

    let opened = Instant::now();
    let mut connection = Connection::open(&database)?;
    initialize(&connection)?;
    let sqlite_version: String =
        connection.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
    let sqlite_open_us = opened.elapsed().as_micros();

    let replaced = Instant::now();
    replace_snapshot(&mut connection, &source_unit, "sha256:content-v1", &batch)?;
    replace_snapshot(&mut connection, &source_unit, "sha256:content-v2", &batch)?;
    let snapshot_replace_us = replaced.elapsed().as_micros();
    drop(connection);

    let reopened = Instant::now();
    let connection = Connection::open(&database)?;
    assert_snapshot_is_queryable(&connection, &source_unit, &batch)?;
    let reopen_and_query_us = reopened.elapsed().as_micros();

    let encoded_at = Instant::now();
    let mut ndjson = serde_json::to_vec(&batch)?;
    ndjson.push(b'\n');
    let ndjson_encode_us = encoded_at.elapsed().as_micros();

    let decoded_at = Instant::now();
    let decoded: AnalysisBatchProbe = serde_json::from_slice(&ndjson)?;
    let ndjson_decode_us = decoded_at.elapsed().as_micros();
    assert_eq!(decoded, batch);

    println!(
        "{}",
        serde_json::to_string(&ProbeMetrics {
            sqlite_version,
            sqlite_open_us,
            snapshot_replace_us,
            reopen_and_query_us,
            ndjson_encode_us,
            ndjson_decode_us,
            ndjson_bytes: ndjson.len(),
        })?
    );

    Ok(())
}

fn initialize(connection: &Connection) -> rusqlite::Result<()> {
    let journal_mode: String =
        connection.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    assert_eq!(journal_mode, "wal");
    connection.execute_batch(
        "
        PRAGMA foreign_keys=ON;
        PRAGMA synchronous=FULL;
        PRAGMA busy_timeout=5000;

        CREATE TABLE IF NOT EXISTS source_snapshots (
            source_unit_id TEXT PRIMARY KEY,
            content_fingerprint TEXT NOT NULL,
            schema_version INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS symbols (
            symbol_id TEXT PRIMARY KEY,
            source_unit_id TEXT NOT NULL,
            name TEXT NOT NULL,
            name_start_byte INTEGER NOT NULL,
            record_json TEXT NOT NULL
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
            target_symbol_id TEXT NOT NULL,
            record_json TEXT NOT NULL,
            PRIMARY KEY (source_unit_id, start_byte, target_symbol_id)
        );
        CREATE INDEX IF NOT EXISTS references_by_target
            ON reference_edges(target_symbol_id, source_unit_id, start_byte);
        ",
    )
}

fn replace_snapshot(
    connection: &mut Connection,
    source_unit: &SourceUnitId,
    content: &str,
    batch: &AnalysisBatchProbe,
) -> Result<(), Box<dyn std::error::Error>> {
    let transaction = connection.transaction()?;
    delete_file_owned_facts(&transaction, source_unit)?;

    transaction.execute(
        "INSERT INTO source_snapshots (source_unit_id, content_fingerprint, schema_version)
         VALUES (?1, ?2, ?3)",
        params![source_unit.as_str(), content, CANONICAL_SCHEMA_VERSION],
    )?;

    for symbol in &batch.symbols {
        transaction.execute(
            "INSERT INTO symbols (symbol_id, source_unit_id, name, name_start_byte, record_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                symbol.id.as_str(),
                symbol.declaration.source_unit.as_str(),
                &symbol.name,
                i64::try_from(symbol.name_range.bytes.start)?,
                serde_json::to_string(symbol)?,
            ],
        )?;
    }

    for occurrence in &batch.occurrences {
        transaction.execute(
            "INSERT INTO occurrences
                 (source_unit_id, start_byte, end_byte, occurrence_kind, target_symbol_id, record_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                occurrence.range.source_unit.as_str(),
                i64::try_from(occurrence.range.bytes.start)?,
                i64::try_from(occurrence.range.bytes.end)?,
                format!("{:?}", occurrence.kind),
                occurrence.target.as_ref().map(SymbolId::as_str),
                serde_json::to_string(occurrence)?,
            ],
        )?;
    }

    for reference in &batch.references {
        transaction.execute(
            "INSERT INTO reference_edges (source_unit_id, start_byte, target_symbol_id, record_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                reference.source.range.source_unit.as_str(),
                i64::try_from(reference.source.range.bytes.start)?,
                reference.target.as_str(),
                serde_json::to_string(reference)?,
            ],
        )?;
    }

    transaction.commit()?;
    Ok(())
}

fn delete_file_owned_facts(
    transaction: &Transaction<'_>,
    source_unit: &SourceUnitId,
) -> rusqlite::Result<()> {
    for table in [
        "reference_edges",
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

fn assert_snapshot_is_queryable(
    connection: &Connection,
    source_unit: &SourceUnitId,
    expected: &AnalysisBatchProbe,
) -> Result<(), Box<dyn std::error::Error>> {
    let persisted_content: String = connection.query_row(
        "SELECT content_fingerprint FROM source_snapshots WHERE source_unit_id = ?1",
        params![source_unit.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(persisted_content, "sha256:content-v2");

    let symbol_json: String = connection.query_row(
        "SELECT record_json FROM symbols WHERE name = ?1",
        params!["pay"],
        |row| row.get(0),
    )?;
    assert_eq!(
        serde_json::from_str::<SymbolRecord>(&symbol_json)?,
        expected.symbols[0]
    );

    let interval_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM occurrences
         WHERE source_unit_id = ?1 AND start_byte <= ?2 AND end_byte > ?2",
        params![source_unit.as_str(), 30_i64],
        |row| row.get(0),
    )?;
    assert_eq!(interval_count, 1);

    let reference_json: String = connection.query_row(
        "SELECT record_json FROM reference_edges WHERE target_symbol_id = ?1",
        params![expected.symbols[0].id.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(
        serde_json::from_str::<ReferenceEdge>(&reference_json)?,
        expected.references[0]
    );

    Ok(())
}

fn probe_batch(source_unit: &SourceUnitId) -> AnalysisBatchProbe {
    let symbol = SymbolRecord {
        id: SymbolId::new("kotlin:app:com.example.PaymentService#pay(kotlin.String)"),
        backend_key: BackendKey {
            backend: "kotlin-k2".to_owned(),
            schema_version: 1,
            value: "opaque-pay-key".to_owned(),
        },
        language: Language::Kotlin,
        kind: SymbolKind::Function,
        name: "pay".to_owned(),
        qualified_name: Some("com.example.PaymentService.pay".to_owned()),
        signature: Some("pay(kotlin.String): kotlin.Unit".to_owned()),
        component: ComponentId::new("gradle::app:main"),
        declaration: SourceRange {
            source_unit: source_unit.clone(),
            bytes: ByteRange { start: 10, end: 50 },
        },
        name_range: SourceRange {
            source_unit: source_unit.clone(),
            bytes: ByteRange { start: 20, end: 23 },
        },
        owner: Some(SymbolId::new("kotlin:app:com.example.PaymentService")),
        freshness: Freshness::Fresh,
        completeness: Completeness::Complete,
        provenance: provenance(),
    };
    let occurrence = SourceOccurrence {
        range: SourceRange {
            source_unit: source_unit.clone(),
            bytes: ByteRange { start: 30, end: 33 },
        },
        kind: OccurrenceKind::Call,
        enclosing_symbol: Some(SymbolId::new("kotlin:app:com.example.Caller#go()")),
        target: Some(symbol.id.clone()),
        type_id: Some(TypeId::new("kotlin:kotlin.Unit")),
        precision: Precision::Exact,
        freshness: Freshness::Fresh,
        completeness: Completeness::Complete,
        provenance: provenance(),
    };
    let reference = ReferenceEdge {
        source: occurrence.clone(),
        target: symbol.id.clone(),
        precision: Precision::Exact,
    };

    AnalysisBatchProbe {
        protocol_version: PROTOCOL_VERSION,
        request_id: "probe-1".to_owned(),
        symbols: vec![symbol],
        occurrences: vec![occurrence],
        references: vec![reference],
        calls: Vec::new(),
    }
}

fn provenance() -> Provenance {
    Provenance {
        backend: "kotlin-k2".to_owned(),
        backend_version: "2.4.10".to_owned(),
        protocol_version: PROTOCOL_VERSION,
        analysis_options: Fingerprint::new("sha256:analysis-options"),
    }
}
