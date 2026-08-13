# KIDE --- Headless Semantic Code Platform

## 1. Product summary

**KIDE** is a lightweight, headless, project-wide code intelligence
platform.

Its goal is to provide IntelliJ-like knowledge about a codebase without
requiring a full IDE process or GUI. KIDE maintains a persistent index
of source code, project modules, dependencies, symbols, references,
types, calls and inheritance, and exposes that model through CLI, LSP,
MCP and a stable programmatic API.

The initial target is **Kotlin/JVM**, with Kotlin K2/Analysis API used
as the semantic authority where full language semantics are required.
The architecture should allow additional language backends later.

KIDE is not intended to reimplement an entire IDE. It extracts the most
valuable part of an IDE --- the project-wide semantic code model --- and
makes it lightweight, headless, programmable and reusable by developers,
editors, agents and CI.

## 2. Problem

Modern IDEs know declarations and references, exact symbol resolution,
types and overloads, inheritance, callers/callees, source and binary
dependencies, refactorings, inspections, quick fixes and build-system
project models. This knowledge is usually coupled to a large interactive
IDE process.

Language servers solve editor integration, but LSP itself is primarily
an editor-facing protocol. It is not an ideal general-purpose API for
shell workflows, coding agents, CI analysis, organization-specific
inspections, complex project transformations or direct querying of a
persistent semantic model.

Structural AST indexers are lightweight, but normally cannot reliably
answer semantic questions such as which overload a call resolves to,
which extension is selected, or which symbol inside a dependency JAR is
referenced.

KIDE aims to occupy the space between these systems.

## 3. Vision

The semantic code model is the product. Editors, CLI, MCP and CI are
clients of it.

Typical usage:

``` bash
kide index .
kide symbol com.example.PaymentService
kide definition src/main/kotlin/Foo.kt:42:17
kide refs com.example.PaymentService.process
kide callers com.example.PaymentService.process
kide implementations com.example.PaymentProvider
kide type-at src/main/kotlin/Foo.kt:71:14
kide inspect
kide rename com.example.Foo.oldName newName --diff
kide move-class com.example.old.User com.example.user.User --dry-run
```

The same engine serves editors through LSP, agents through MCP/CLI/API,
and CI through inspections and machine-readable reports.

## 4. Goals and non-goals

### Goals

1.  Maintain one project-wide model rather than isolated file analysis.
2.  Index both project source and external dependencies.
3.  Persist indexes across restarts.
4.  Incrementally update affected data after changes.
5.  Provide exact semantic information where possible.
6.  Answer common queries with low latency.
7.  Work without a GUI.
8.  Expose first-class CLI and API interfaces.
9.  Support LSP as an editor frontend.
10. Support programmable inspections, fixes and refactorings.
11. Allow project- and organization-specific extensions.
12. Eventually support multiple languages.

### Non-goals for initial versions

KIDE does not initially attempt to provide a complete IntelliJ
replacement, debugger UI, VCS UI, database tooling, Android UI tooling,
every IntelliJ inspection/refactoring, or a new Kotlin compiler frontend
written in Rust.

## 5. Positioning

### KIDE vs LSP

LSP and KIDE are not direct alternatives. LSP is an editor protocol;
KIDE is the engine beneath those operations.

LSP is document/position oriented:

``` text
textDocument/definition
textDocument/references
textDocument/hover
textDocument/completion
textDocument/rename
```

KIDE's native API is project/symbol oriented:

``` text
findReferences(SymbolId)
findCallers(SymbolId)
findImplementations(SymbolId)
typeOf(ExpressionId)
rename(SymbolId, Name)
moveClass(SymbolId, PackageId)
runInspection(InspectionId)
runOperation(OperationId, Args)
```

An LSP adapter translates document positions into KIDE symbols and
returns LSP results.

### KIDE vs IntelliJ IDEA

IntelliJ already has a sophisticated project model, indexes, PSI,
language semantics, inspections and refactorings. KIDE trades breadth
for headless operation, lower resource use, server/CI friendliness,
first-class CLI, agent-friendly APIs, programmable operations and
potential multi-repository analysis.

The goal is not to beat IDEA as an interactive IDE. The goal is to make
semantic/code-model capabilities independently available.

### KIDE vs IDEA MCP

IDEA MCP exposes capabilities of an already running IDE:

``` text
Agent -> MCP -> IntelliJ -> IDEA indexes / PSI / Kotlin plugin
```

KIDE owns the code-intelligence service:

``` text
CLI ─┐
MCP ─┼──> KIDE
LSP ─┤
CI  ─┘
```

No interactive IDE needs to be running.

### KIDE vs AST indexers

AST indexers provide structural navigation. KIDE extends this toward
compiler-aware code intelligence:

``` text
AST -> symbol index -> project model -> semantic resolution
    -> types / overloads / inheritance -> safe transformations
```

KIDE should know what a reference means, not merely what syntax it
resembles.

## 6. High-level architecture

``` text
                    KIDE CORE
+------------------------------------------------+
| Project Model                                  |
| Persistent Index                               |
| Syntax Model                                   |
| Semantic Model                                 |
| Query Engine                                   |
| Inspection Engine                              |
| Refactoring / Edit Engine                      |
| Extension Runtime                              |
| Incremental Invalidation Engine                |
+-----------------------+------------------------+
                        |
               Language Backend API
                        |
          +-------------+-------------+
          |             |             |
      JVM/Kotlin       Rust       TypeScript
          |             |             |
        K2/AA      rust-analyzer   compiler API
          |
     Kotlin + Java
                        |
                 Client/API layer
                        |
          +-------------+-------------+
          |             |             |
         CLI           LSP           MCP
```

## 7. Core data model

KIDE uses stable IDs and compact data-oriented representations rather
than exposing compiler object graphs as its persistent model.

Core identities include `ProjectId`, `ModuleId`, `LibraryId`, `FileId`,
`SymbolId`, `TypeId`, `StringId`, `DiagnosticId` and `OperationId`.

A symbol contains language, name, kind, owner, module, file, source
range and flags. Symbol kinds include class, interface, object,
function, method, constructor, property, field, parameter, package,
module and type alias.

A reference connects a source/location to a target symbol and has a
reference kind.

The project graph represents modules, source/generated roots, compiler
options, module dependencies, libraries, artifacts and SDK/JDK
information.

## 8. Source and dependency indexing

Dependencies are first-class members of the semantic universe.

For a call through `JdbcTemplate`, KIDE should resolve the local
variable to its type, the fully-qualified dependency class, the
containing JAR and ultimately the exact `query` overload.

The index should include project source, generated source, project
modules, JDK, Kotlin stdlib, dependency JARs and source JARs when
available.

Binary dependencies should be indexed once and reused rather than
repeatedly loaded or decompiled.

## 9. Syntax layer

KIDE needs a lossless or sufficiently lossless syntax representation for
safe edits. It should preserve comments, whitespace/token ranges,
annotations, imports and delimiters.

This layer enables operations such as formatting, sorting imports,
adding/removing imports or annotations, changing visibility and deleting
declarations without always requiring full semantic analysis.

## 10. Semantic layer

Kotlin resolution includes overloads, extension functions, implicit
receivers, generic substitution, smart casts, aliases, delegated
properties, operators, companion objects, Java interop, callable
references and JVM-specific rules.

KIDE should therefore not reimplement Kotlin semantics from scratch. The
initial JVM backend should use Kotlin K2/Analysis API as the semantic
authority.

``` text
                Rust KIDE daemon
                       |
       +---------------+---------------+
       |               |               |
 Project Model      Index DB        Syntax DB
       |               |               |
       +---------------+---------------+
                       |
                  Query Engine
                       |
              semantic required?
                 /           \
               no             yes
               |               |
             Rust          K2 worker
                               |
                         Analysis API
```

The JVM/K2 worker should be long-lived. Cheap queries should be served
directly from the persistent index when correctness permits.


### Semantic workers as lazy, disposable compute

The concrete ownership, persistence, lifecycle, and query rules for this model
are recorded in [ADR 0001: Cold, Disposable Backend Workers](docs/architecture/0001-cold-disposable-backend-workers.md).

Language semantic engines should be treated as **compute workers**, not as the owners of KIDE's persistent project knowledge.

For the Kotlin/JVM backend, K2/Analysis API is required when KIDE must derive trustworthy semantic facts from source, including:

- name and reference resolution;
- expression and receiver types;
- overload selection;
- extension/implicit receiver resolution;
- generic substitution;
- smart casts;
- Kotlin/Java interoperability;
- compiler diagnostics;
- semantic validation of selected transformations.

Once derived, reusable semantic facts should be normalized and persisted in KIDE's own index, for example:

```text
CallSite -> resolved SymbolId
Reference -> target SymbolId
Expression -> TypeId
Symbol -> implementations
Symbol -> callers/callees
```

Read-oriented queries should then use the Rust-side persistent index whenever the required facts are already fresh. `references`, `callers`, `implementations`, hierarchy queries, domain queries and many inspections should not require a live K2 process merely to read previously computed facts.

Conceptually:

```text
source/config changed
        |
        v
 semantic worker
    K2 / JVM
        |
        v
 normalized semantic facts
        |
        v
 KIDE persistent index
        ^
        |
 Rust query engine
```

This makes the compiler frontend closer to a semantic index builder/validator than the runtime database behind every query.

#### Worker lifecycle policies

KIDE should support different semantic-worker lifecycle policies for different workloads:

```text
CLI / CI:
  Rust daemon always available
  semantic worker starts on demand
  worker may exit after an idle timeout

Agent:
  Rust daemon always available
  semantic worker starts on demand
  worker may remain warm for a configurable period

Interactive IDE:
  Rust daemon always available
  semantic worker normally remains warm while the project is active
```

Possible configuration:

```toml
[semantic-worker]
mode = "on-demand"
idle-timeout = "60s"
```

Candidate modes:

- `always-on` — optimized for interactive editor latency;
- `on-demand` — optimized for CLI/agent workloads;
- `off` — structural/index-only mode where semantic guarantees are not required.

The exact process and memory characteristics of K2 must be measured rather than assumed. The architecture should make it possible to reclaim worker memory without losing the persistent KIDE index.

#### Lazy semantic recomputation

A source edit does not necessarily require immediate semantic recomputation.

If a syntax-level update shows that a function body changed while its declaration summary, imports and relevant API surface remain unchanged, KIDE may update structural facts immediately and defer expensive semantic work until required.

Semantic facts should therefore have explicit freshness state, conceptually:

```text
FRESH
  known to match the current project snapshot

STALE
  previously computed but invalidated by a relevant change

UNKNOWN
  not yet computed for this snapshot
```

Queries that require correctness should request fresh facts and wake the appropriate language worker if necessary. Specialized exploratory queries may optionally permit stale data when explicitly requested, but stale results must never be silently presented as current semantic truth.

#### Multi-language consequence

The same lifecycle model should apply to future language backends:

```text
                 KIDE Core / Rust
                        |
          +-------------+-------------+
          |             |             |
       Kotlin        TypeScript      other
       K2/JVM        worker          worker
          |             |             |
          +------ lazy/disposable ----+
```

KIDE should not need to keep every compiler/language server process resident merely because a multi-language repository is open.

This reinforces the architectural boundary:

> **Persistent semantic knowledge belongs to KIDE; language engines are replaceable, lazily activated authorities used to derive or validate that knowledge.**


## 11. Incremental architecture

Performance depends primarily on invalidation architecture, not merely
implementation language.

KIDE distinguishes changes to function bodies, declaration signatures,
imports, public API, module dependencies and build configuration.

Changing a function body without changing its signature should update
body/local-reference information without invalidating global knowledge
about the declaration.

A useful model separates per-file declaration/API summaries from
function/property bodies.

## 12. Persistent index

The persistent index optimizes for compact representation, fast startup,
cheap lookup, sequential access, optional memory mapping and incremental
updates.

Important relationships include:

``` text
name -> symbols
symbol -> declaration
symbol -> references
symbol -> callers/callees
symbol -> implementations
symbol -> subclasses/supertypes
file -> symbols
module -> files
module -> dependencies
library -> symbols
annotation -> symbols
```

Rust is attractive here because it supports compact data structures,
predictable ownership and data-oriented representations.

## 13. Query engine

Core queries are independent of frontend protocols:

``` text
symbol(query)
definition(location)
references(SymbolId)
callers(SymbolId)
callees(SymbolId)
implementations(SymbolId)
subclasses(SymbolId)
supertypes(SymbolId)
typeAt(location)
resolve(location)
symbolsIn(FileId)
dependencies(ModuleId)
dependents(ModuleId)
```

CLI, MCP and LSP delegate to the same operations.

## 14. Edit and refactoring engine

All modifications should preferably become explicit `WorkspaceEdit`
objects before application. A workspace edit can contain text edits,
file creation, file moves and deletion.

This enables dry runs, diffs, transactional validation, editor previews,
agent review and CI automation.

Initial operations can progress by complexity:

-   formatting and organize imports;
-   add/remove import or annotation;
-   change visibility;
-   rename symbol;
-   move class/package;
-   change signature;
-   extract interface;
-   inline/extract function.

A move-class operation should update the file location/package, semantic
references, imports and qualified names, then validate visibility and
formatting.

## 15. Inspection engine and quick fixes

An inspection has metadata and requirements and analyzes a context to
produce diagnostics.

A diagnostic contains a location, severity, message, optional symbol and
zero or more fixes. A fix is effectively an operation producing a
`WorkspaceEdit`.

Generic examples include unused declarations and deprecated API usage.
Architecture-specific examples include Controller-to-Repository access,
HTTP calls inside transactions, forbidden dependencies, required Kafka
keys or blocking calls inside suspend functions.

Outputs should include human-readable text, JSON and SARIF for CI.

## 16. Extension system

Programmability is a core differentiator.

Extensions may register:

-   queries that read the semantic/project model;
-   inspections that produce diagnostics and fixes;
-   operations that produce transformations.

The extension API exposes symbols, references, calls, hierarchy, types,
project graph and edit construction.

A repository may contain:

``` text
.kide/
  config.toml
  inspections/
    no-http-in-transaction
    kafka-key-required
  operations/
    migrate-api-v2
    move-to-feature-module
```

Execution models may eventually include built-in Rust, WASM and an
external-process protocol. The stable extension boundary is more
important than requiring plugins to use Rust.

## 17. Frontends

### CLI

CLI is first-class:

``` bash
kide index .
kide status
kide symbols Payment
kide refs PaymentService.pay
kide callers PaymentService.pay
kide impl PaymentProvider
kide inspect
kide operations
kide rename ... --diff
kide move-class ... --dry-run
```

### Daemon

A long-lived `kided` owns the project model, persistent indexes, caches,
file watcher and semantic workers. CLI commands connect over local IPC,
allowing near-instant command startup.

### LSP

LSP is the compatibility frontend for VS Code, Zed and other editors. It
maps URI/position requests onto the KIDE symbol/query model.

### MCP

MCP exposes agent-friendly semantic tools. It should be a thin adapter
rather than the internal architecture.

## 18. Multi-language architecture

KIDE Core should be language-neutral where practical.

A `LanguageBackend` supplies language-specific parsing and semantic
capabilities such as symbol extraction, reference resolution, type
queries and validation.

The core provides persistent storage, indexing, query orchestration,
inspections, operations and frontend protocols.

KIDE should reuse mature semantic engines rather than rewrite every
compiler frontend. Potential backends include K2 for Kotlin,
rust-analyzer for Rust and TypeScript compiler APIs for TypeScript.

Kotlin and Java should initially be treated as a combined JVM ecosystem
because Kotlin projects inherently interact with Java source, JDK
classes and Java libraries.

## 19. Cross-language model

A longer-term advantage is a shared project/architecture graph across
languages.

Language-level references remain the responsibility of language
backends, while extensions can create higher-level relations such as:

``` text
Kotlin REST provider <-> TypeScript REST consumer
Kafka publisher <-> consumer
database table <-> repositories
generated API <-> implementation
```

This allows inspections and queries that no single-language LSP can
naturally answer.

## 20. Why Rust

Rust is useful primarily for the infrastructure layer:

-   compact persistent indexes;
-   large symbol/reference sets;
-   predictable memory use;
-   fast traversal;
-   parallel indexing;
-   file watching;
-   low-overhead daemon and CLI;
-   memory-mapped storage;
-   safe concurrency.

Rust is not expected to make Kotlin semantic resolution inherently
faster. The larger performance gains should come from architecture:
incremental computation, correct invalidation, compact data and caching.

Therefore the intended split is:

``` text
Rust:
  orchestration
  storage
  structural index
  project graph
  query engine
  extension runtime
  CLI/LSP/MCP

Kotlin/K2:
  expensive Kotlin-specific semantic truth
```

## 21. MVP

The first useful product should avoid GUI work.

### Phase 1 --- structural daemon

-   Rust daemon and CLI;
-   Gradle/JVM project discovery;
-   Kotlin/Java source indexing;
-   JAR dependency indexing;
-   persistent symbol index;
-   declarations/imports;
-   basic references;
-   incremental file updates;
-   symbol/search/dependency CLI.

### Phase 2 --- semantic JVM backend

-   long-lived K2 worker;
-   exact definition resolution;
-   semantic references;
-   type-at-position;
-   implementations;
-   hierarchy;
-   callers/callees;
-   semantic cache.

### Phase 3 --- edits

-   lossless syntax/edit model;
-   formatter integration;
-   organize imports;
-   rename;
-   move class/package;
-   dry-run/diff/apply workflow.

### Phase 4 --- programmable platform

-   inspection API;
-   quick fixes;
-   custom operations;
-   extension SDK/protocol;
-   JSON/SARIF;
-   CI integration.

### Phase 5 --- editor and agent frontends

-   LSP;
-   MCP;
-   optional lightweight editor integration.

### Phase 6 --- additional languages

Validate the language abstraction with a substantially different
ecosystem such as Rust or TypeScript.


## Distributed semantic artifacts and index registry

KIDE indexes should eventually be treated as **distributable derived artifacts**, not only as local IDE caches.

The core idea is:

> **If an analysis result depends only on immutable artifact content and compatible analysis settings, compute it once and reuse it everywhere.**

This is particularly valuable for package dependencies, which are normally immutable for a released version and heavily shared across projects, worktrees, developers, CI workers and coding agents.

### Content identity over package version

Package coordinates are useful metadata, but they should not be the authoritative identity of a reusable index.

For example:

```text
org.springframework:spring-core:6.x
                  |
                  v
          spring-core.jar
                  |
             SHA-256 = ABC
                  |
                  v
          LibraryIndex(ABC)
```

The content hash is authoritative because package coordinates can theoretically point to different bytes, and mutable/SNAPSHOT packages make this explicit.

A library artifact record may therefore contain:

```text
LibraryArtifact
├── content hash
├── ecosystem coordinates
├── package/version metadata
├── language/backend
└── semantic index reference
```

Two projects using identical artifact bytes reuse the same immutable library index regardless of repository or worktree.

### Shared dependency semantic indexes

Dependency analysis can be split into two layers.

The first layer is artifact-local and globally reusable:

```text
Immutable dependency artifact
            |
            v
    LibrarySemanticIndex
            |
   symbols / signatures
   hierarchy / annotations
   package/module structure
```

The second layer is project-context-dependent:

```text
Project classpath + source + compiler configuration
                    |
                    v
           resolution facts
```

Facts such as “this JAR contains class X, method Y and hierarchy edge Z” depend only on the artifact and backend/index format. Facts such as “this call in PaymentService resolves to X.Y” depend on the project context and belong to the project/worktree semantic snapshot.

### Remote KIDE Index Registry

KIDE may provide or integrate with a remote registry of precomputed semantic artifacts.

Lookup flow:

```text
dependency artifact
       |
       v
compute content hash
       |
       v
local KIDE CAS?
   |          |
  yes         no
   |          |
 reuse        v
        remote registry?
          |          |
         yes         no
          |           |
      download     index locally
          |           |
          +-----+-----+
                |
                v
            local CAS
```

A remote index manifest should bind the derived index to the exact source artifact and analysis compatibility information, for example:

```text
sourceArtifactHash
indexFormatVersion
language
languageBackendVersion
analysisOptionsFingerprint
```

KIDE must verify that the downloaded index was produced for the exact artifact bytes before trusting it.

### Ecosystem-independent design

The model should not be JVM-specific.

Examples:

```text
Maven/Gradle JAR
      -> content hash
      -> JVM semantic package index

npm package/tarball
      -> content hash
      -> TypeScript/JavaScript package index

Cargo crate
      -> content hash
      -> Rust package index
```

Package-manager coordinates remain metadata and discovery keys; content identity remains the cache identity.

### Publishing models

Several distribution models are possible.

A library author could publish a semantic side artifact alongside binaries:

```text
library.jar
library-sources.jar
library.kide
```

However, KIDE should not require library authors to participate. An independent KIDE registry can discover popular released artifacts and precompute their indexes.

For private ecosystems, organizations can operate an internal registry next to Nexus/Artifactory or another package repository:

```text
              Company package registry
                       |
                artifact published
                       |
                       v
                  KIDE indexer
                       |
                       v
                Company KIDE CAS
                       |
          +------------+------------+
          |            |            |
      Developers       CI         Agents
```

This is particularly attractive for internal libraries because one semantic index can serve the entire organization.

### Lazy/chunked indexes

A later optimization may split a large semantic artifact into independently fetchable components, for example:

```text
symbols
signatures
hierarchy
annotations
sources/structural data
```

A client could download only the portions needed for a query and fetch additional chunks lazily.

The MVP should prefer a simpler single-artifact format until measurements justify chunking.

### Project snapshots as distributable artifacts

The same mechanism can eventually extend beyond dependencies.

CI may publish a KIDE snapshot for a particular Git commit:

```text
Git commit ABC
      |
      v
CI builds/validates KIDE snapshot
      |
      v
Remote KIDE CAS / registry
      |
      +---- dependency indexes
      +---- source structural indexes
      +---- compatible semantic facts
```

A developer or coding agent checking out the same commit can retrieve the compatible snapshot and create only a local dirty-worktree overlay.

This can significantly reduce cold-start indexing for large repositories and ephemeral agent environments.

### Relationship to build caches

The registry is conceptually similar to a remote build cache:

```text
input identity + tool/version/configuration
                  |
                  v
             derived artifact
```

The difference is that the artifact contains code-intelligence/semantic knowledge rather than compiled outputs.

This suggests a long-term architecture in which indexing is no longer repeated independently by every IDE process. Expensive immutable analysis becomes shared infrastructure.

### Registry principle

The long-term rule is:

> **KIDE indexes are build-like semantic artifacts: content-addressed, verifiable, cacheable, shareable and optionally publishable.**

Local indexing remains the fallback, so remote registry availability must not be required for normal operation.


## 22. Key technical risks

### Kotlin semantics

Full Kotlin resolution is complex. Mitigation: treat K2 as semantic
authority instead of reimplementing it.

### Gradle project import

Accurately reproducing classpaths, generated sources, compiler flags and
multi-module configuration is a substantial subsystem.

### Incremental invalidation

Over-invalidation destroys performance; under-invalidation produces
stale/incorrect answers. The dependency model and cache keys are core
architecture.

### Stable symbol identity

Symbols need identities that survive routine edits where possible while
invalidating correctly after semantic changes.

### Rust/JVM boundary

Frequent fine-grained RPC into K2 can eliminate performance gains.
Semantic calls should be batched, cached and invoked only when the
Rust-side index cannot answer safely.

### Editing correctness

Structural edits are easy to prototype but semantic refactorings must
handle visibility, imports, overloads, generated code and build files.

### Extension safety

Custom operations can modify large portions of a repository. Dry-run,
explicit edits, validation and transactional application are essential.

## 23. Product principles

1.  **Headless first.** GUI is a client, never a prerequisite.
2.  **Project-wide by default.** The unit of understanding is the
    project/module graph.
3.  **Dependencies are code.** External libraries belong in the
    searchable semantic universe.
4.  **Persistent by default.** Do expensive work once and reuse it.
5.  **Incremental by design.** Avoid global recomputation.
6.  **Semantic when necessary, structural when sufficient.** Do not
    invoke expensive compiler analysis unnecessarily.
7.  **One engine, many clients.** CLI, LSP, MCP and CI share the same
    model.
8.  **Edits are data.** Transformations produce reviewable workspace
    edits before mutation.
9.  **Programmability is first-class.** Organization-specific knowledge
    should be expressible as inspections and operations.
10. **Reuse language authorities.** KIDE coordinates semantic engines
    rather than attempting to become every compiler.
11. **Rust is an implementation tool, not the product thesis.**
    Architecture creates the performance advantage.
12. **Agents are first-class consumers.** APIs should expose semantic
    intent rather than force agents to manipulate source text.

## 24. Success criteria

A successful early KIDE should make it possible to clone/open a large
Kotlin/JVM project, build its persistent project/dependency index, stop
and restart the daemon without full reindexing, and answer common
symbol/reference/hierarchy queries quickly from CLI.

It should resolve dependency symbols and Kotlin references accurately
enough that an agent can navigate a project without repeatedly reading
whole files.

Later success means semantic refactorings and custom inspections can run
headlessly with previewable edits, making the same code-intelligence
engine useful to developers, editors, agents and CI.

## 25. One-sentence definition

> **KIDE is a headless, persistent, programmable semantic code platform:
> an IntelliJ-like project model exposed as CLI/API infrastructure, with
> LSP and MCP as frontends rather than as the core architecture.**
