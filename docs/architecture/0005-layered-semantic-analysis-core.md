# ADR 0005: Layered Semantic Analysis Core

- Status: Proposed
- Date: 2026-08-16
- Scope: reusable Core and worker contracts for control-flow and dataflow
  analyses
- Related: [ADR 0001](0001-cold-disposable-backend-workers.md),
  [ADR 0003](0003-lazy-semantic-graph-expansion.md),
  [ADR 0004](0004-semantic-relation-query-contract.md)

## Context

The semantic selector MVP persists resolved declarations and relations. It can
answer bounded relational questions such as a nested declaration whose owner
carries a resolved application with a named argument. Queries such as "which
`InputStream` values can leave a callable unclosed?" need more: control-flow,
state propagation, ownership transfer, and eventually summaries across calls.

These capabilities must remain framework- and language-neutral in Core while
allowing each worker to use its native semantic authority. Kotlin FIR, Java
bytecode, Rust MIR, and TypeScript control-flow do not have interchangeable
internal representations.

## Decision

KIDE will grow semantic analysis in seven explicitly separated layers. A
layer consumes normalized outputs of earlier layers; it must preserve
provenance, precision, freshness, completeness, and explicit resource bounds.
Core owns persistence, planning, composition, and result/evidence rendering.
Workers own language resolution and construction of language-specific source
or bytecode facts. A worker never becomes a persistent query database or runs
an arbitrary Core-supplied semantic DSL.

| Layer | Responsibility | Current KIDE status | Required next contract |
| --- | --- | --- | --- |
| 1. Semantic facts | Stable symbols, source ranges, types, ownership, applications, references, calls, hierarchy. | Implemented for the Kotlin/JVM MVP; application instances and typed arguments are persisted. | Extend relation capability advertisement; add normalized acquire/release/transfer facts only where they are direct language facts. |
| 2. Relational patterns | Bounded indexed joins, predicates, language views, result metadata and proofs. | Implemented for resolved applied-symbol postings and the first nested application/owner/argument join. | Generalize the application join into the relation-pattern surface described by ADR 0004; retain mandatory indexed starting sets. |
| 3. Program segments | On-demand representation of a callable body: blocks, normal/exceptional edges, operations and lexical exits. | Not implemented. Existing workers return declaration-level facts, not CFGs. | `CallableSegmentRequest` and `CallableControlFlowGraph` with explicit budgets, snapshot ownership, and source ranges. |
| 4. Intraprocedural state | Fixed-point propagation of a finite state through one CFG; aliases and scopes are modelled within the bounded callable. | Not implemented. | A Core analysis input model for states, transfer operations and terminal paths; first consumer is resource ownership. |
| 5. Effect summaries | Compact per-callable facts usable by callers: closes/consumes/borrows/transfers/returns resource ownership, sanitizes taint, may throw. | Not implemented. | Versioned `CallableEffectSummary`, keyed by callable identity, semantic input fingerprint and analysis configuration. |
| 6. Interprocedural analysis | Compose summaries and call edges to a bounded fixed point, with recursion and dispatch policy made explicit. | Call edges are persisted, but no interprocedural solver or summary planner exists. | A demand-driven planner with maximum call depth, units, nodes, bytes, iterations and deadline; partial result on exhaustion. |
| 7. Whole-program approximation | Build/configuration-aware dispatch, dependency boundaries, callbacks, async, DI/reflection and generated code. | Artifact catalog/materialization and exact resolved calls provide prerequisites; no whole-program approximation exists. | Worker capabilities and explicit assumptions for call-graph, points-to, reflection and async models. |

## Delivery plan by consumer

The rollout is organised around the response budget and evidence expected by
the caller, not around a claim to complete all seven layers before shipping a
useful feature. An LSP request must never implicitly trigger unbounded
whole-program analysis.

| Priority | Layers | Agent outcome | LSP / IDE outcome | Completion gate |
| --- | --- | --- | --- | --- |
| P0: semantic map | 1–2 | Deterministic definitions, references, types, impact sets, framework patterns, and proof facts in JSONL. | Incremental navigation and persisted compiler/worker diagnostics; inspections based on direct semantic facts. | Every result begins from a bounded posting or explicitly reports no/partial result; facts are snapshot-owned and explainable. |
| P1: local reasoning | 3–4 | Explain a path through one callable, including normal and exceptional exits. | On-demand local inspections: unreachable code, constant conditions, redundant guards, nullability checks, and local resource lifecycle. | `CallableControlFlowGraph` materializes only a selected callable; every diagnostic has a source-range evidence path and a bounded analysis budget. |
| P1.5: reusable contracts | 5 | Answer whether a helper borrows, closes, transfers, returns, sanitizes, or can throw without reopening its body. | Better local diagnostics at calls to known APIs and annotated project methods. | Summaries carry semantic input/configuration fingerprints and can be invalidated independently from callers. |
| P2: bounded flows | 6 | Follow resource ownership, taint, or API effects through a requested, limited call chain. | Explicit user/agent action or background analysis; never a mandatory keystroke-time inspection. | The planner exposes maximum depth, targets, nodes, iterations, bytes and deadline; exhausted work is `partial`, with the unresolved boundary named. |
| P3: audit mode | 7 | Repository/CI audit over DI, dispatch, callbacks, async, reflection, dependencies, and generated code. | Batch inspection with a profile and stated modelling assumptions. | Per-language call-graph and runtime-model capabilities are declared; results separate proven findings from approximation-dependent findings. |

### Serving policy

- **Editor keystroke and ordinary LSP navigation:** use cached P0 facts and
  worker/compiler diagnostics only. A stale or unavailable semantic snapshot
  is surfaced instead of blocking the UI.
- **Save, explicit inspection, and agent exploration:** permit lazy P1 CFG
  materialization for the selected callable or a small candidate set.
- **Explicit agent request, CI, or repository audit:** permit P2/P3 only with
  a supplied or profile-selected budget, entry points, and assumptions.

This policy makes the same Core useful to all consumers without making the
lowest-latency consumer pay for the most expensive analysis.

### Sequencing decisions

1. Complete P0 before adding a general CFG schema: it is the common semantic
   map needed by both agents and inspection result rendering.
2. Introduce the Layer-3 segment contract before choosing a permanent
   dataflow engine. It fixes the Core/worker boundary independently of a
   JVM-only implementation.
3. Ship one P1 analysis, resource ownership, with a deliberately
   intraprocedural scope before generalising to taint or nullability.
4. Add Layer-5 summaries before Layer-6 traversal; otherwise every helper
   call reopens a body and cost becomes unpredictable.
5. Treat SootUp evaluation as a P2 JVM-worker spike. It must demonstrate
   export of the Layer-3/5 contracts rather than introduce Jimple into Core.

### Boundary by layer

Workers produce Layers 1 and 3 from their native semantic systems. They may
also produce direct Layer-5 summaries when they can prove them locally. Core
persists these immutable snapshot-owned outputs, executes Layer 2, plans and
materializes Layer 3 on bounded candidates, and owns the solvers/planners for
Layers 4 and 6 when the normalized representation is sufficient.

A worker-specific engine may calculate an internal dataflow result, but it
returns only normalized facts, summaries, evidence and an explicit answer
state. Core does not receive PSI, FIR, Jimple, MIR, or arbitrary AST objects.

### First analysis: resource ownership

The first typestate analysis will model a resource identity with states such
as `acquired`, `released`, `transferred`, `escaped`, and `unknown`. Its output
is not binary:

- `definite_leak`: a supported path reaches a terminal exit while acquired;
- `safe`: supported paths prove release or an accepted ownership transfer;
- `partial`: an unresolved call, alias, dispatch target, reflection boundary,
  or budget limit prevents a complete conclusion.

The initial scope is one callable. It recognises source-language equivalents
of acquisition, explicit close/release, scope-managed release (for example
Java try-with-resources and Kotlin `use`), `return`, and known local transfer.
Interprocedural ownership depends on Layer 5 and is deliberately deferred.

### SootUp and other analysis frameworks

SootUp may be used inside the JVM worker to build bytecode CFGs, class
hierarchies, call graphs, or JVM-specific dataflow summaries. It is not a Core
dependency and Jimple is not the KIDE canonical IR. Source-first Kotlin
analysis remains a Kotlin worker concern; a bytecode engine is especially
useful for dependencies and Java-only regions.

The same rule applies to Rust MIR, TypeScript compiler CFGs, or any future
language framework: they are replaceable worker implementations behind the
Layer-3/5 contracts, not part of Core's persistence schema.

## Consequences

- The current selector model remains useful and is not inflated into a
  general dataflow language.
- Every advanced result carries assumptions and `partial` state instead of
  claiming a whole-program proof.
- New languages can contribute at different precisions: a worker may first
  support Layers 1–2, then add callable segments and summaries later.
- Core contracts should be introduced before choosing a permanent JVM
  framework so SootUp can be evaluated as an implementation detail rather
  than an architectural dependency.

## Deferred decisions

- The concrete canonical CFG operation set and its binary representation.
- Whether the Layer-4 solver is embedded in Rust Core or is a separately
  supervised analysis worker.
- Context sensitivity, points-to precision, callback/async modelling, and
  reflection policy for each language.
- A user-facing query language for analyses. The initial API may use named
  analysis kinds and parameter objects rather than exposing a universal DSL.
