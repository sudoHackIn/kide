# ADR 0001: Cold, Disposable Backend Workers

- Status: Accepted
- Date: 2026-08-13
- Scope: language backends, build-system adapters, persistent indexing, query execution

## Context

KIDE is language- and build-system-agnostic, but exact analysis depends on
ecosystem-specific authorities such as Kotlin K2, rust-analyzer, the TypeScript
compiler, Gradle, Maven, or Cargo.

Keeping every authority resident would make the KIDE daemon a supervisor for a
large collection of compiler and build processes. It would also make common
read-only navigation queries depend on worker availability, startup state, and
memory lifetime. That conflicts with KIDE's goal of owning a lightweight,
persistent semantic code model.

At the same time, KIDE cannot replace those authorities with a single generic
AST. A generic tree can describe syntax but cannot reliably resolve overloads,
extensions, generics, implicit receivers, build variants, conditional
dependencies, or other language- and build-specific semantics.

## Decision

Language and build-system backends are **cold, disposable compute workers**.
They do not own KIDE's project knowledge and are not required to remain alive
after indexing.

KIDE Core is the durable owner of:

- the workspace, component, source-unit, and dependency graph;
- normalized symbols, source occurrences, references, calls, types, and
  hierarchy edges;
- configuration and content fingerprints;
- freshness and invalidation state;
- persistent indexes and the query engine.

Workers temporarily own:

- parser, AST, CST, or PSI instances;
- compiler and analysis sessions;
- imported build-tool models;
- backend-specific caches that have not been normalized into KIDE facts.

A worker starts on demand, analyzes a batch, emits a versioned snapshot or
delta, and becomes eligible for shutdown. Destroying it must not discard facts
already committed to the KIDE index.

```text
                    long-lived
 +----------------------------------------------+
 | KIDE Core                                    |
 | project graph | persistent index | queries   |
 | freshness     | invalidation      | storage  |
 +----------------------+-----------------------+
                        | start on demand
              +---------+----------+
              | backend worker     |
              | AST/PSI/compiler   |
              | build import       |
              +---------+----------+
                        | manifest / snapshot / delta
                        v
                  transactional commit
                        |
                  worker may stop
```

## Backend contracts

### Build-system worker

A build adapter produces a versioned `ProjectManifest`:

```text
ProjectManifest
  workspace identity
  components and source sets
  source, generated, resource, and excluded roots
  component dependency edges
  immutable dependency artifacts
  toolchains and SDKs
  compiler/backend options
  configuration fingerprints
```

KIDE persists the manifest. Gradle, Maven, Cargo, Bazel, or another importer is
only restarted when its inputs have changed or the manifest is missing or
incompatible.

### Language worker

A language worker consumes source units plus their component context and emits
a versioned `FileAnalysisSnapshot` or `AnalysisDelta`:

```text
FileAnalysisSnapshot
  source unit identity
  content and context fingerprints
  declarations and stable backend keys
  source occurrences and ranges
  resolved references and calls
  hierarchy edges
  type facts
  diagnostics
  public API and dependency fingerprints
  completeness and provenance
```

The protocol must be batch-oriented. KIDE must not require one process call or
RPC round-trip per symbol or source occurrence.

The payload is normalized and language-neutral where the concepts are truly
shared. Language-specific details use explicitly versioned opaque fields rather
than forcing all languages into a universal AST.

## Persistence boundary

KIDE does not persist a compiler object graph as its primary index. It persists
compact canonical records, including:

```text
SymbolRecord
SourceOccurrence
ReferenceEdge
CallEdge
HierarchyEdge
TypeRecord
DiagnosticRecord
```

Every file-owned fact is attributable to a `SourceUnitId` and analysis
snapshot. Re-indexing a file transactionally replaces the facts owned by the
previous snapshot.

An optional serialized syntax representation may be added later if benchmarks
show that it materially improves edits or indexing. It is an optimization, not
the canonical semantic model.

## Query behavior while workers are cold

Common navigation queries operate on persistent indexes:

```text
symbols(name)          name index -> symbols
definition(location)  occurrence interval -> target -> declaration
references(symbol)    reverse reference index -> occurrences
callers(symbol)       reverse call index -> enclosing symbols
implementations(sym)  reverse hierarchy index -> symbols
typeAt(location)      occurrence interval -> type
```

A worker is not started merely because a query is semantic. It is started only
when the required facts are absent, stale, incompatible, or the operation
intrinsically requires live backend state.

## Freshness and wake-up policy

Persisted fact sets have an explicit state:

- `FRESH`: valid for the current source and project-context fingerprints;
- `STALE`: previously computed, but a relevant input changed;
- `UNKNOWN`: never computed for the current snapshot;
- `UNSUPPORTED`: the selected backend cannot provide the requested fact.

Correctness-critical queries never present stale facts as current truth. A
query against stale or unknown data either:

1. starts the appropriate worker and waits for a fresh transactional update;
2. returns an explicitly marked partial/stale result when the caller opted into
   that behavior; or
3. returns a structured unsupported or analysis error.

Worker policy is configurable without changing backend contracts:

```toml
[workers.kotlin]
mode = "on-demand"
idle-timeout = "30s"
```

During `kide index`, a worker remains alive for the analysis batch. During a
burst of agent queries it may stay warm until the idle timeout. It may then stop
without affecting already indexed queries.

## Invalidation

Structural and semantic cache identities are separate:

```text
structural key =
  source content hash
  + parser/backend version
  + parse options

semantic key =
  structural fingerprint
  + component context fingerprint
  + dependency graph fingerprint
  + compiler/backend version
  + analysis options
```

A file watcher or explicit command marks affected data stale; it does not need
to eagerly start a worker. Re-analysis may be eager during `kide index` or lazy
on the first correctness-critical query.

The first MVP may invalidate more than strictly necessary, provided it remains
correct. It may re-analyze a component after an API or classpath change while
limiting body-only changes to the changed file. Finer invalidation follows from
measurements.

## Capability discovery

Backend capability metadata is static and can be read without starting the
worker. It declares support and expected precision for declarations,
definitions, references, calls, hierarchy, types, diagnostics, formatting, and
edits.

KIDE must distinguish exact, approximate, and unsupported results. Text matches
must never be silently returned as exact semantic references.

## Failure and transaction rules

- Worker failure leaves the last committed snapshot intact but marks affected
  facts stale or failed for the requested refresh.
- Partial worker output is not exposed as a complete snapshot.
- Snapshot commits verify source and context fingerprints to prevent committing
  analysis for files that changed during the run.
- Protocol and index format versions are explicit compatibility inputs.
- Worker logs and provenance identify backend name, version, configuration, and
  completeness.

## Consequences for the semantic-navigation MVP

The MVP must establish the persistence and worker boundary before adding many
queries. Its first vertical slice is:

```text
project discovery
  -> ProjectManifest
  -> Kotlin/JVM worker batch
  -> FileAnalysisSnapshot
  -> persistent symbol/occurrence/reference indexes
  -> cold-worker CLI queries
```

The required CLI surface is:

```bash
kide index .
kide status
kide symbols PaymentService
kide definition src/main/kotlin/Foo.kt:42:17
kide refs src/main/kotlin/Foo.kt:42:17
kide implementations src/main/kotlin/PaymentProvider.kt:10:11
kide callers src/main/kotlin/PaymentService.kt:31:9
kide type-at src/main/kotlin/Foo.kt:50:14
```

Location-oriented lookup is first-class because agents usually know the file
and position before they know a stable `SymbolId`. Symbol-oriented forms use the
same query engine and are provided where useful.

## Consequences

### Positive

- common queries remain fast and available with cold language workers;
- compiler and build memory can be reclaimed;
- KIDE owns a frontend-independent semantic model;
- workers can be upgraded or replaced behind versioned protocols;
- local and future remote semantic artifacts share the same persistence model.

### Costs

- snapshots, provenance, freshness, and invalidation must be designed early;
- KIDE must normalize backend output without erasing language-specific truth;
- exact indexing requires batch materialization before cold queries are useful;
- stale-data handling becomes part of every query contract.

## Non-goals of this decision

Storage and local worker transport are selected in
[ADR 0002: SQLite and NDJSON for the Semantic-Navigation MVP](0002-sqlite-and-ndjson-for-mvp.md).
This ADR does not select a parser or compiler version. It also does not require
workers to terminate after every individual request; batching and a bounded idle
timeout are compatible with a cold-worker architecture.
