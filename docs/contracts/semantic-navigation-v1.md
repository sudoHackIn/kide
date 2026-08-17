# Semantic Navigation Contract v1

- Status: accepted for the semantic-navigation MVP
- Canonical schema version: `1`
- Rust source of truth: `crates/kide-core/src/{canonical,query}.rs`
- Related architecture: [ADR 0001](../architecture/0001-cold-disposable-backend-workers.md)

## Purpose and boundary

This contract defines the persistent, language-neutral facts KIDE owns and the
agent-facing queries over them. It is deliberately not a universal AST and not
a serialization of compiler objects.

Language workers may use PSI, compiler trees, language servers, or another
native representation while alive. Before a worker stops it normalizes its
output into KIDE records. Build workers likewise normalize their output into a
`ProjectManifest`. A cold worker is therefore not required for a fresh query
whose facts are already in the persistent index.

```text
build worker    -> ProjectManifest
language worker -> source-unit facts and semantic edges
                         ↓
                persistent KIDE graph
                         ↓
       CLI / MCP / future API query engine
```

## Versioning and compatibility

Every machine-readable response has a required top-level `schema_version`. The
physical index also has its own `index_format_version`; storage migration is
independent of a response-schema change.

- A reader may ignore unknown fields in a known schema version.
- A producer must not change the meaning or type of an existing field in place.
- A breaking response or canonical-record change increments
  `schema_version`; a reader receiving a newer incompatible version returns
  `unsupported_schema` rather than guessing.
- A backend key is comparable only when its `backend` and
  `schema_version` both match. Its `value` is opaque to KIDE Core.
- A fact is reusable only when its source/content, context, backend version,
  analysis-options fingerprint, and relevant project/dependency fingerprints
  remain compatible.

## Canonical graph

The first MVP persists these records and relations.

| Record | Owned by KIDE | Required role |
| --- | --- | --- |
| `ProjectManifest` | Yes | Workspace components, dependencies, toolchain/configuration fingerprint. |
| `SourceUnit` | Yes | One file/binary-derived unit in a component context. |
| `SymbolRecord` | Yes | Declaration identity, ownership, language, signature, location. |
| `SourceOccurrence` | Yes | Interval-index entry at a declaration, reference, call, type reference, or import. |
| `ReferenceEdge` | Yes | Exact source occurrence to target symbol relation. |
| `CallEdge` | Yes | Exact selected callable and optional enclosing caller. |
| `HierarchyEdge` | Yes | Subtype/implementation to supertype relation. |
| `TypeRecord` | Yes | Normalized type identity plus language-specific display form. |
| `BackendKey` | Yes, opaque | Backend-local stable identity carried without KIDE interpreting it. |
| AST/PSI/compiler session | No | Ephemeral worker-local computation state. |

The common model intentionally supports both Kotlin/JVM and non-JVM workers:

```json
{
  "id": "typescript:ui:src/main.ts",
  "component": "npm:ui",
  "path": "ui/src/main.ts",
  "language": "type_script",
  "origin": "source",
  "content": "sha256:98c9…",
  "context": "sha256:2165…"
}
```

No JVM classpath, PSI node, JVM descriptor, or Gradle-only field is required in
`SourceUnit`, `SymbolRecord`, or a query envelope. Backend-specific detail is
kept in a versioned `BackendKey` or an explicit future extension payload.

`ProjectManifest.Component` may additionally carry build-model data required
to create a cold language analysis session: `source_sets` (main/test source and
generated roots), resolved external `classpath` content fingerprints, portable
`toolchain` version data, and a `compiler_configuration` fingerprint. These
fields are optional/defaulted for filesystem fallback manifests. They never
store an absolute JDK path or a live Gradle/PSI object.

## Paths, locations, and ranges

All persisted source paths are normalized, slash-separated paths relative to
the workspace root. Absolute paths and paths escaping through `..` are rejected
before they enter the canonical index. The CLI accepts a user path relative to
its current directory, resolves it to the workspace, and returns this
workspace-relative form.

Location input is position-oriented:

```text
PATH:LINE:COLUMN
src/main/kotlin/Foo.kt:42:17
```

- `LINE` and `COLUMN` are one-based.
- `COLUMN` counts Unicode scalar values, not UTF-8 bytes and not UTF-16 code
  units.
- CLI/API locations name a cursor position. A lookup chooses the narrowest
  indexed `SourceOccurrence` containing that cursor; when equal ranges exist,
  declaration/reference/call/type-reference precedence is command-specific and
  documented in tests.
- Persisted `SourceRange.bytes` is a half-open UTF-8 byte interval `[start,
  end)` into the source unit's exact content snapshot. This makes interval
  indexing compact and unambiguous while keeping the user contract Unicode-safe.
- A lookup fails with `invalid_location` when the path, line, column, or source
  snapshot is invalid. It never rounds a position into a multi-byte character.

## Fact quality

Every result includes quality metadata:

```json
{
  "freshness": "fresh",
  "completeness": "complete",
  "precision": "exact",
  "index_format_version": 1,
  "source_snapshot": "sha256:98c9…",
  "provenance": [
    {
      "backend": "kotlin-k2",
      "backend_version": "2.4.10",
      "protocol_version": 1,
      "analysis_options": "sha256:2165…"
    }
  ]
}
```

`freshness` is one of:

- `fresh`: inputs match the current compatible project snapshot;
- `stale`: a relevant input changed after the fact was recorded;
- `unknown`: the fact has not been computed for this snapshot;
- `unsupported`: no selected backend can provide this fact.

`completeness` is `complete`, `partial`, or `failed`. `precision` is `exact` or
`approximate`.

Default semantic-navigation commands require `fresh` and `exact` facts. KIDE
may later add an explicit `--allow-stale` or `--allow-approximate`; it must
never silently return text matches as exact semantic references.

## Query input

`definition`, `refs`, `implementations`, and `callers` accept a
`NavigationTarget` in one of three forms:

```json
{"kind":"location","location":{"path":"src/Foo.kt","position":{"line":42,"column":17}}}
{"kind":"symbol","symbol":"kotlin:app:com.example.Foo#run()"}
{"kind":"query","query":"com.example.Foo.run"}
```

The CLI starts with the location syntax because agents commonly know a file and
cursor but not a stable `SymbolId`. Symbol queries are convenience lookup only:
an ambiguous query returns candidates and does not arbitrarily select one.

| Command | Canonical request | Successful payload |
| --- | --- | --- |
| `kide index <path>` | `index` | indexed and changed source-unit counts |
| `kide status` | `status` | manifest freshness, source-unit freshness counts, running workers |
| `kide symbols <query>` | `symbols` | deterministic symbol list |
| `kide definition <target>` | `definition` | one `SymbolRecord` |
| `kide refs <target>` | `refs` | reference occurrences, not textual matches |
| `kide implementations <target>` | `implementations` | implementation/subtype symbols |
| `kide callers <target>` | `callers` | resolved call occurrences |
| `kide type-at <location>` | `type_at` | narrowest occurrence and `TypeRecord` |

`implementations` is direct-only by default. `--transitive` additionally
follows descendant hierarchy edges in deterministic breadth-first order, with
each discovered subtype returned once. `callers` reports the selected semantic
call target, therefore it distinguishes overloads and extension/member calls.

## Response envelope and errors

KIDE selects output based on stdout: when stdout is a terminal, navigation
commands render concise human-readable declarations or `path:line:column`
locations; when stdout is redirected or connected to a pipe, they emit one
versioned JSON `QueryResponse` on stdout. `--json` forces the latter in a
terminal. `--short` explicitly requests the compact location-oriented human
format. This makes an interactive command readable while keeping pipelines
such as `kide symbols Name | kide definition | kide ref` structured without
flags. Logs and worker diagnostics go to stderr.

### Selector JSONL pipelines

`kide select --applies <resolved-symbol-id>` emits one UTF-8 JSON record per
selected declaration when stdout is piped. Each record contains the full
canonical `SymbolRecord` and its freshness, completeness, and provenance
metadata; consumers use `symbol.id`, never rendered declaration text. The
optional `--kotlin-class`, `--java-class`, `--component`, and `--qualified-prefix` filters are
conjunctive. `refs`, `callers`, and `implementations` accept this JSONL stream,
deduplicate SymbolIds, and process them in stable lexical ID order. A selector
with no records exits `1`; a fan-out navigation command exits `0` if any target
has a result and `1` if none do. One navigation response is emitted per input
target, so a consumer can preserve attribution without reparsing text.

```json
{
  "schema_version": 1,
  "status": "ok",
  "result": {
    "kind": "definition",
    "symbol": {
      "id": "kotlin:app:com.example.PaymentService#pay(kotlin.String)",
      "backend_key": {
        "backend": "kotlin-k2",
        "schema_version": 1,
        "value": "opaque-worker-key"
      },
      "language": "kotlin",
      "kind": "function",
      "name": "pay",
      "qualified_name": "com.example.PaymentService.pay",
      "signature": "pay(kotlin.String): kotlin.Unit",
      "component": "gradle::app:main",
      "declaration": {
        "source_unit": "kotlin:app:src/main/kotlin/PaymentService.kt",
        "bytes": {"start": 217, "end": 320}
      },
      "name_range": {
        "source_unit": "kotlin:app:src/main/kotlin/PaymentService.kt",
        "bytes": {"start": 221, "end": 224}
      },
      "owner": "kotlin:app:com.example.PaymentService",
      "freshness": "fresh",
      "completeness": "complete",
      "provenance": {
        "backend": "kotlin-k2",
        "backend_version": "2.4.10",
        "protocol_version": 1,
        "analysis_options": "sha256:2165…"
      }
    }
  },
  "metadata": {
    "freshness": "fresh",
    "completeness": "complete",
    "precision": "exact",
    "index_format_version": 1,
    "source_snapshot": "sha256:98c9…",
    "provenance": []
  },
  "problems": []
}
```

`status` is exactly one of `ok`, `no_result`, `ambiguous`, `stale`,
`unsupported`, `invalid_request`, or `failed`. `problems` carries structured
codes such as `ambiguous_symbol`, `invalid_location`, `index_missing`,
`facts_stale`, `worker_failed`, and `unsupported_capability`.

CLI exit codes for v1:

| Exit | Meaning |
| --- | --- |
| `0` | `ok` |
| `1` | `no_result` or `ambiguous` |
| `2` | `invalid_request` |
| `3` | `stale` or `unsupported` without an allowed fallback |
| `4` | `failed` |

## Determinism

Every collection returned in v1 is sorted by normalized workspace path, byte
range start, then stable ID. Candidate lists returned for ambiguity use the same
ordering. This is required for agent reproducibility, fixture tests, and stable
JSON diffs.

## Deferred contract

`FileAnalysisSnapshot`, `AnalysisDelta`, worker handshake fields, IPC framing,
storage transactions, and migration mechanics belong to the backend-protocol
and storage tasks. They must serialize the canonical records defined here;
they do not redefine their meaning.

For the MVP's selected local storage and worker transport, see
[ADR 0002](../architecture/0002-sqlite-and-ndjson-for-mvp.md).
