# ADR 0003: Lazy Semantic Graph Expansion

- Status: Proposed
- Date: 2026-08-15
- Scope: dependency artifacts, semantic facts, selector queries, worker wake-up
- Related: [ADR 0001](0001-cold-disposable-backend-workers.md),
  [ADR 0002](0002-sqlite-and-ndjson-for-mvp.md),
  [semantic-navigation contract v1](../contracts/semantic-navigation-v1.md)

## Context

The initial Kotlin/JVM index proves that persistent, disk-backed navigation is
fast: a cold worker can produce immutable artifact blobs, Core can materialize
their normalized facts, and later queries do not need compiler state in memory.
However, eagerly materializing every class in every resolved dependency is the
wrong steady-state trade-off. A small application can bring hundreds of JARs,
hundreds of thousands of symbols, and a very large SQLite index even though an
agent will inspect only a small part of that graph.

At the same time, the future query surface is richer than name navigation. For
example, "Kotlin classes annotated with Spring `RestController`" needs an
annotation relation and an indexed posting; it cannot reliably be answered by
searching display strings in serialized symbol records. KIDE deliberately does
not persist a universal AST, so these capabilities must be expressed as
normalized graph facts rather than PSI-specific traversal APIs.

## Decision

KIDE will keep immutable source and dependency analysis blobs as the durable
authoritative output of workers, but materialize their normalized graph facts
into SQLite **on demand**. The SQLite index is a query-serving cache and catalog,
not an unconditional copy of every fact in every blob.

### Tiers of data

1. **Catalog facts are eager and small.** They identify sources, artifacts,
   content/context fingerprints, provenance, supported capabilities, and blob
   locations. They let the planner determine which blobs could answer a query.
2. **Direct semantic facts are materialized by demand.** A requested symbol,
   artifact, or selector candidate materializes its declarations, locations,
   direct references/calls/types/hierarchy facts, and indexed postings needed
   for that request. The worker is started only if no compatible blob exists.
3. **Derived graph expansion is bounded.** Traversal beyond direct facts carries
   an explicit frontier, requested relation kinds, and a budget (depth, nodes,
   bytes, and time). It records its completeness so a partial result is never
   presented as a universal answer.

This is a semantic depth, not a syntax-tree depth. A Kotlin/Java annotation,
TypeScript decorator, or Python decorator is represented as a direct
`applied_symbol` relation from a declaration to a resolved symbol.
`callers` may require one reverse-call hop. A question such as "controllers
that eventually invoke X" has unbounded call depth and therefore requires a
caller-selected bound or a precomputed application rule.

### Normalized graph and selector boundary

Core remains framework-neutral. Workers emit language-neutral facts where they
are exact, including:

```text
symbol --applied_symbol--> symbol
symbol --references/calls--> symbol
symbol --extends/implements--> symbol
source occurrence --has_type--> type
```

Raw source spelling is not a selector fact. Fully-qualified resolved applied
symbols and other selector predicates receive relational
postings so the planner can start with a narrow candidate set rather than scan
and decode every blob. Language-specific semantic objects are bounded views
over those facts (for example `kotlin.class` and `java.class`), not
framework-specific Core commands or persisted PSI.

The first selector proof is conjunction of reusable predicates: symbol kind,
resolved applied symbol, qualified-name prefix, language, component, and
provenance. The Spring CRUD fixture is test data only: `BookController`,
`BookEntity`, and transactional methods prove the same generic primitive.

### Primitive relation coverage

| Relation / predicate | Status | Use |
| --- | --- | --- |
| `kind`, `language`, `component`, qualified-name prefix, provenance | Implemented | Bounded declaration filtering. |
| `applied_symbol` | Implemented | Resolved annotation/decorator-style modifiers with reverse postings. |
| `references`, `calls`, `hierarchy`, `has_type` | Implemented | Direct semantic navigation and bounded traversal. |
| `provides` | Planned | A framework view may expose a produced runtime capability, such as a DI binding. |
| `requests` | Planned | A framework view may expose a dependency/injection request. |
| `binds` | Planned | A resolved `requests -> provides` edge; it must preserve exact, ambiguous, conditional, or unresolved state. |

No framework command or framework-specific persistent type belongs in Core.
Views compile to these primitives and report their inference precision.

### Worker and cache policy

An immutable blob is keyed by artifact/source identity, content hash, analysis
backend version, context, and capability set. A query proceeds as follows:

```text
query -> catalog/postings -> materialized facts if sufficient -> answer
                           -> compatible blob -> materialize required segment
                           -> no compatible blob -> start worker -> blob -> segment
```

Materialization is transactional, idempotent, and attributable to its blob.
Evicting materialized SQLite rows is safe because they can be restored from the
blob; deleting blobs is governed by cache policy separately. Platform classes
(for example JDK `jrt` modules) must become first-class catalog artifacts too,
so an external hierarchy target is not mistaken for a navigable declaration.

Configuration may declare application-specific rules that justify prewarming
or deeper expansion. Otherwise, expensive multi-hop traversal runs only when a
query asks for it. The planner may return a bounded partial answer with explicit
freshness/completeness instead of silently scanning all dependencies.

## Consequences

- Fast navigation continues to use the persistent store and does not require a
  resident compiler or an in-memory whole-program graph.
- Initial indexing and disk use scale with catalog/blob production rather than
  all resolved dependency symbols.
- Query latency may include a one-time segment materialization; results and
  logs must say when that happened.
- Demand materialization must not read a complete artifact blob or decoded
  `GraphArtifact` into memory. The cache blob remains the verified immutable
  hand-off, but Core validates and reads the requested graph section as a
  stream of length-delimited snapshot records and inserts them through batched
  SQLite statements in one transaction. A failed record rolls that transaction
  back. This is an optimization of demand loading, not source indexing.
- Worker protocols need capability-aware segment requests, not only whole-file
  snapshots. The format remains versioned so a future protobuf descriptor/blob
  layout can evolve independently of Core's canonical facts.
- Completeness, invalidation, provenance, and resource budgets become required
  parts of every derived graph result.

## Follow-up

- Add indexed resolved-annotation facts and a bounded selector planner
  ([kide-ilj.24]).
- Define JSONL records and pipeline failure semantics for agent composition
  ([kide-ilj.18]).
- Add lazy artifact and platform-module materialization, measured against the
  Spring CRUD fixture before replacing the current eager dependency path.
- Make graph-fact blob sections streamable and bulk-insert their snapshots
  during bounded demand materialization; measure peak memory and load latency
  against the Spring CRUD fixture.
