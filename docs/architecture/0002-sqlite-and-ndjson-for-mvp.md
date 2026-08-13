# ADR 0002: SQLite and NDJSON for the Semantic-Navigation MVP

- Status: Accepted
- Date: 2026-08-14
- Scope: local persistent index storage and Rust/JVM worker transport
- Related: [ADR 0001](0001-cold-disposable-backend-workers.md),
  [semantic-navigation contract v1](../contracts/semantic-navigation-v1.md)

## Context

KIDE's MVP must persist normalized symbols, occurrences, references, calls,
hierarchy, types, project manifests, fingerprints, freshness, and provenance.
The index must survive a worker shutdown and process restart. Re-indexing one
source unit must atomically replace all facts owned by its previous snapshot.

KIDE Core is Rust; the first semantic worker is Kotlin/JVM. The worker boundary
must send batches rather than one request per occurrence, remain inspectable
while the protocol evolves, and not require a resident service or generated
code toolchain before semantic navigation works.

## Decision

The MVP uses:

- **SQLite 3.53.x via `rusqlite` with the `bundled` feature** for the local
  persistent index;
- **one local database per workspace**, initially at `.kide/index-v1.sqlite3`;
- **WAL journal mode**, `foreign_keys=ON`, `busy_timeout=5000`, and
  `synchronous=FULL` as initial connection policy;
- **one writer transaction per accepted file snapshot**, which deletes old
  file-owned rows and inserts the replacement facts before `COMMIT`;
- **newline-delimited UTF-8 JSON (NDJSON)** over a worker's stdin/stdout for
  request, response, progress, and error messages;
- **one message per batch**, with a protocol version and request ID. A batch
  contains many `FileAnalysisSnapshot` records and never requires an RPC per
  symbol/reference/call.

The dependency is resolved by Cargo.lock to `rusqlite 0.40.2`, whose bundled
SQLite is `3.53.2`. This is newer than the SQLite WAL-reset fix in 3.51.3.
Bundling makes KIDE independent of a user's system SQLite version.

```text
KIDE Core                         cold Kotlin worker
---------                         ------------------
SQLite index <--- commit facts --- AnalysisBatch
    ^                                  |
    |                           NDJSON over pipes
    +--------- queries -----------+    |
```

## SQLite schema shape

SQLite stores scalar keys needed by its indexes alongside canonical record JSON
for a narrow MVP implementation:

```text
source_snapshots(source_unit_id, content_fingerprint, schema_version)
symbols(symbol_id, source_unit_id, name, name_start_byte, record_json)
occurrences(source_unit_id, start_byte, end_byte, kind, target_symbol_id, record_json)
reference_edges(source_unit_id, start_byte, target_symbol_id, record_json)
```

The next storage task will add calls, hierarchy, types, manifests, migrations,
and complete integrity constraints. It must retain these access patterns:

```text
name -> symbols                 symbols_by_name
location -> occurrence          occurrences_by_source_interval
symbol -> references            references_by_target
symbol -> callers               calls_by_target
symbol -> implementations       hierarchy_by_supertype
```

Canonical byte offsets are `u64`; the SQLite adapter must use checked conversion
to signed 64-bit integers and reject an unrepresentable value. It must never
silently truncate an offset.

SQLite WAL allows readers to proceed while a writer commits, but it permits only
one writer and requires local shared memory. KIDE's local per-workspace index is
therefore not placed on a network filesystem and the daemon serializes write
transactions. SQLite documents both the reader/writer benefits and these WAL
constraints: [WAL mode](https://www.sqlite.org/wal.html).

`synchronous=FULL` is selected initially for conservative commit durability.
The index is rebuildable, so this can be measured and relaxed later only through
an explicit durability/performance decision.

## NDJSON worker protocol

Each message occupies exactly one newline-terminated UTF-8 JSON value. Top-level
fields are stable:

```json
{
  "protocol_version": 1,
  "request_id": "01J…",
  "kind": "analysis_batch",
  "payload": {
    "snapshots": ["…many canonical FileAnalysisSnapshot values…"]
  }
}
```

Rules for the protocol task:

- stdout is protocol-only; structured logs go to stderr;
- a newline, nesting-depth, and decoded-message-size limit are enforced before
  allocating unbounded payloads;
- unknown fields can be ignored only within a compatible protocol version;
- unknown `kind`, invalid JSON, a duplicate request ID, or a version mismatch
  produces a structured error message;
- output is deterministic for a fixed request;
- only the Core commits a successfully validated batch; a worker never opens
  KIDE's SQLite database.

JSON is stable in Kotlin through `kotlinx-serialization-json` and in Rust via
Serde. Kotlin documents JSON as its stable serialization format, while its CBOR
and Protocol Buffer modules remain experimental; that makes JSON the lower-risk
MVP boundary. [Kotlin serialization formats](https://kotlinlang.org/docs/serialization.html)

## Bounded proof

The runnable proof is:

```bash
make storage-transport-spike
# or
cargo run -p kide-core --example storage_transport_spike
```

It uses the actual canonical `SymbolRecord`, `SourceOccurrence`, and
`ReferenceEdge` types to:

1. create a temporary WAL SQLite index;
2. transactionally replace one source unit's snapshot twice;
3. reopen the database and perform name, interval, and reverse-reference
   lookups;
4. encode and decode the same multi-record batch as one NDJSON message;
5. emit only JSON timing/size metrics.

One development-machine run produced:

```json
{
  "sqlite_version": "3.53.2",
  "sqlite_open_us": 6060,
  "snapshot_replace_us": 1633,
  "reopen_and_query_us": 1155,
  "ndjson_encode_us": 147,
  "ndjson_decode_us": 79,
  "ndjson_bytes": 1898
}
```

These are smoke-measurements, not performance targets. Their purpose is to
prove cold open, transactional replacement, persistent access paths, and batch
serialization before storage/protocol implementation starts.

## Alternatives considered

| Option | Decision | Reason |
| --- | --- | --- |
| `redb` / custom KV indexes | Reject for MVP | Would require KIDE to design transactions, reverse indexes, interval access, migrations, and ad-hoc query planning concurrently. Reconsider only after SQLite profile data identifies a concrete bottleneck. |
| RocksDB | Reject for MVP | Strong KV engine, but adds native operational weight and leaves relation/interval modeling to KIDE without benefiting the one-local-index MVP. |
| PostgreSQL | Reject for MVP | Requires a service lifecycle and is inappropriate for a self-contained cold-start local index. |
| JSON files | Reject for MVP | Human-readable but cannot give transactional replacement and efficient reverse/interval queries without rebuilding whole files. |
| CBOR / Protocol Buffers over pipes | Defer | More compact, but adds schema/code-generation coordination. Kotlin currently calls these serialization formats experimental. Retain a future `encoding` field behind a protocol-version bump. |
| gRPC / sockets | Defer | Useful for future long-lived remote workers, but more lifecycle and endpoint machinery than disposable local child processes need. |
| one JSON message per fact | Reject | Violates batching and amplifies Rust/JVM round-trip overhead. |

## Migration and operational rules

- The physical index format has a separate `index_format_version` from the
  canonical and worker protocol versions.
- Use ordered, idempotent SQL migrations recorded in a `schema_migrations`
  table. Never edit a released migration.
- A failed migration preserves a backup or causes an index rebuild; it never
  serves mixed-version facts.
- A file snapshot commit verifies content and context fingerprints immediately
  before commit. If inputs changed, the transaction rolls back and the unit
  remains stale.
- The worker process can fail, stop, or be upgraded without invalidating an
  already committed compatible snapshot.
- KIDE's default index path is local; a remote registry later distributes
  immutable artifacts, not a live WAL database.

## Consequences

The storage task can now implement a deliberately relational MVP without
locking the Core to a compiler or build tool. The protocol task can make a
single transparent, debuggable Rust/JVM bridge first, then introduce a compact
encoding only when measurements justify it.
