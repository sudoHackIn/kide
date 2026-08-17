# ADR 0004: Semantic Relation and Worker Query Contract

- Status: Accepted
- Date: 2026-08-15
- Scope: framework-neutral semantic relations, composable queries, worker capabilities
- Related: [ADR 0001](0001-cold-disposable-backend-workers.md),
  [ADR 0003](0003-lazy-semantic-graph-expansion.md)

## Context

KIDE needs queries such as "nested configurations conditional on a property"
and "Rust handlers carrying a resolved web-framework macro". These cannot be
reliably expressed as source-text searches, and a framework command per query
would put Spring/Rust knowledge in Core. GritQL is a useful ergonomics model:
bindings, conjunction, disjunction, negation and `where` conditions. Its
primary data model is a language AST, however; KIDE's is a persistent graph of
resolved semantic identities across files and artifacts.

## Decision

Core will expose a declarative, bounded relation pattern language. A pattern
binds graph nodes, traverses typed relations, filters properties, and returns
named bindings plus proof facts. It does not execute worker code or accept an
arbitrary AST query. Workers advertise which normalized relations they can
produce and answer a versioned request for missing bounded facts.

### Node kinds

| Node | Identity | Purpose |
| --- | --- | --- |
| `symbol` | stable `SymbolId` | declarations, types, macros and annotations. |
| `application` | source-unit scoped stable ID | one use of an annotation, decorator, attribute or attribute/proc macro. |
| `value` | typed canonical value | literal, enum constant, list or map argument value. |
| `occurrence` | source range + snapshot | a user-visible reference/call/type site. |

`application` is intentionally separate from `symbol`. A direct
`applied_symbol` posting remains an efficient projection, but it cannot retain
two uses of the same target or their argument values.

### Required direct relations

| Relation | From → to | Notes |
| --- | --- | --- |
| `owns` | symbol → symbol | class/member, nested class/outer class, parameter/function. |
| `applies` | symbol → application | declaration carries a declarative application. |
| `targets` | application → symbol | resolved annotation, decorator or macro symbol. |
| `argument` | application → value | named or positional argument, including its name/index. |
| `references`, `calls` | occurrence/symbol → symbol | existing resolved navigation facts. |
| `has_type` | symbol or occurrence → symbol/type | resolved declared or expression type. |
| `subtype_of` | symbol → symbol | extends/implements/trait relation. |
| `generated_by` | symbol → application | optional; required only when an expansion creates a navigable declaration. |

All relations carry provenance, precision, freshness and completeness. A
worker must distinguish exact facts from an unsupported or partial analysis;
Core never upgrades either to exact.

### Pattern operations

The first version needs only relational operations, not a general language:

```text
match $configuration: symbol
where
  $configuration.kind == class
  $configuration -[owns]-> $outer: symbol
  $configuration -[applies]-> $config_use: application
  $config_use -[targets]-> org.springframework.context.annotation.Configuration
  $outer -[applies]-> $conditional: application
  $conditional -[targets]-> org.springframework.boot.autoconfigure.condition.ConditionalOnProperty
  $conditional -[argument(name="name")]-> "feature.x"
return $configuration, $outer, $conditional
```

Supported operators are binding, typed edge traversal, equality, set-membership,
prefix/regex over explicitly textual properties, `and`/`or`/`not`, projection,
deduplication and explicit `limit`. Traversal has depth/node/byte/time bounds.
Negation is valid only over a materialized and declared-complete candidate set;
otherwise the result is `partial`, not an assertion that no match exists.

The same structure expresses a Rust route without Rust vocabulary in Core:

```text
match $handler: symbol
where
  $handler.kind == function
  $handler -[applies]-> $route: application
  $route -[targets]-> actix_web.get
return $handler, $route
```

Import aliases and spelling differences do not affect either query because
`targets` holds the resolved symbol identity.

### Worker capability and request

Workers advertise relation capabilities, for example
`applications.resolved_target`, `application.arguments`, `ownership`,
`macro_attributes`, and `macro_generated_symbols`. Core plans only against
advertised capabilities.

A future request is declarative and segment-bounded:

```text
SemanticPatternRequest {
  pattern: relation-pattern-v1,
  required_relations: [owns, applies, targets, argument],
  candidate_source_units: [...],
  budget: { max_units, max_nodes, max_bytes, deadline }
}
```

The worker response contains normalized facts owned by each source/artifact
snapshot, plus `complete`, `partial`, or `unsupported` per requested
capability. It does not return PSI/FIR/AST objects and it does not make the
worker a persistent query database.

### Extraction requirements

For every source declaration the backend must extract a stable symbol ID,
owner, kind, language, declaration range, resolved types where available, and
ordinary reference/call/hierarchy facts. For each declarative application it
must extract its subject, resolved target symbol, source range, and resolved
argument values that the backend can prove. Macro expansion facts are opt-in:
an attribute macro itself is already an `application`; generated declarations
need `generated_by` only when they must be navigated as declarations.

## Consequences

- Framework lenses become saved pattern libraries and result renderers, not
  Core commands or schema branches.
- `applied_symbol` continues as a compact reverse posting for the common
  target-only case; application instances are materialized only when a query
  asks for arguments or provenance of the use.
- Blob catalog/loading remains a separate planning concern. It can later index
  which relations/symbols a blob can answer without changing semantic query
  meaning.
- The first implementation should prove the two examples above with fixture
  tests before designing DI/provider inference or macro expansion traversal.
