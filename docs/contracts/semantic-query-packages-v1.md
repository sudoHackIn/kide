# Semantic Query Packages Contract v1

- Status: accepted for the declarative query-packages MVP
- Related architecture: [ADR 0004](../architecture/0004-semantic-relation-query-contract.md)
- Extends: [Semantic Navigation Contract v1](semantic-navigation-v1.md)

## Purpose

KIDE query packages let an agent invoke named, repository-aware semantic
commands without adding framework vocabulary or project-specific commands to
Core. A package is declarative: it supplies macros and typed command templates
written in the bounded semantic relation DSL. Core validates, plans and
executes them against its persistent graph; workers are only asked for a
declared, bounded capability when a future plan requires one.

```text
project command -> package macro -> relation DSL / typed IR -> Core planner
                                                               ↓
                                                    persistent graph postings
                                                               ↓
                                                bounded worker capability (future)
```

The current `kide select` command is a built-in package-shaped proof: an
`applies` posting is its required indexed starting relation. Query packages are
the composable public surface built above the same facts.

## Package sources and resolution

The MVP resolves packages in this order:

1. built-in packages distributed with KIDE;
2. workspace packages under `.kide/query-packages/<package-id>/`;
3. workspace command definitions under `.kide/queries/`.

Workspace command definitions may import only packages that the registry has
already validated. A project definition may add named commands and bind
parameters, but it cannot replace a built-in package or shadow an imported
package-qualified name.

Package identifiers and command names are ASCII lower-case dotted names:
`spring.web`, `payments.controllers`. A command is addressed as
`<package-id>.<command-name>`; a project command is addressed by its declared
name and must be globally unique in the workspace.

## Manifest

Each package contains `package.toml`:

```toml
format = 1
id = "spring.web"
version = "1.0.0"
requires_core = "^1"

[exports]
macros = ["controller"]
commands = ["controllers"]

[[capabilities]]
name = "applications.resolved_target"
required = true

[[capabilities]]
name = "application.arguments"
required = false
```

- `format` changes only when manifest meaning changes incompatibly.
- `requires_core` is checked before loading the package.
- A capability is a versioned, named semantic relation capability, never an
  executable hook. Required unavailable capabilities make a command
  `unsupported`; optional unavailable capabilities produce a documented
  `partial` result only when the selected plan uses them.
- `version` and the canonical manifest digest are carried in resolved-plan
  provenance.

The MVP does not download packages, execute package code, load dynamic
libraries, or permit project configuration to mutate the registry.

## Relation DSL

Package macro and command bodies use the relation-pattern language from ADR
0004. Its implementation is a typed Core IR, not an AST query language and not
a general-purpose programming language.

The MVP surface has the following constructs:

```text
query <name>(<typed parameters>) {
  from <indexed-positive-relation>
  where <predicate> [and|or <predicate> ...]
  return <named symbol bindings>
  limit <positive integer>
}
```

Supported relations and filters are `applies`, `owns`, `targets`, `calls`,
`references`, `subtype_of`, symbol kind, language, component, qualified-name
prefix, equality and typed parameter substitution. Every program must begin
from one positive indexed relation; a bare `from symbol` is invalid. The first
implementation uses `applies(target)` as that relation, preserving the bounded
selector property.

`not` is reserved until the planner can prove that its candidate set is
materialized and complete. Unbounded traversal, arbitrary regular expressions,
file reads, shell execution, reflection and user-defined functions are not
part of v1.

### Types and parameters

Parameters have one of these v1 types:

| Type | Accepted value | Meaning |
| --- | --- | --- |
| `symbol-id` | canonical `SymbolId` | resolved graph identity |
| `qualified-symbol` | exact qualified name | resolved deterministically by the registry/index |
| `component-id` | canonical `ComponentId` | project component boundary |
| `string` | UTF-8 scalar string | only in a typed equality/prefix position |
| `integer` | bounded positive integer | explicit query limit |

Parameter substitution is structural and typed. It never concatenates source
text into a DSL program. Ambiguous qualified symbols are invalid requests;
they are never selected arbitrarily.

## Macros and project commands

A framework macro expands to relation DSL before planning. For example, a
Spring package can define `spring.web.controller()` in terms of the generic
resolved annotation target, rather than teaching Core about Spring:

```text
macro controller() =
  applies(qualified-symbol("org.springframework.web.bind.annotation.RestController"))
  and kind(class)
```

A workspace command stores a parameterized query program under
`.kide/queries/payments.controllers.kql`:

```text
use spring.web

command payments.controllers(component: component-id) {
  from spring.web.controller()
  where component == $component
  return symbol
  limit 100
}
```

The CLI surface is deliberately small:

```text
kide query list
kide query describe payments.controllers
kide query payments.controllers --component gradle:payments-api:main
```

`describe` returns the parameter schema, resolved package versions and static
capability requirements without starting a worker. Invocation emits stable
JSONL `SelectorRecord`-compatible records so existing `refs`, `callers` and
`implementations` fan-out commands can consume them unchanged.

## Planning, bounds and result state

Core compiles macros and project commands to a canonical typed plan before any
query executes. The plan records:

- package ID/version/digest and project-command path/content fingerprint;
- resolved macro expansion and typed parameter bindings;
- indexed starting relation, predicates, projection and explicit limit;
- requested worker capabilities and every budget.

The planner applies default caps for candidates, output records, relation
nodes, bytes and deadline. A package may request lower caps but cannot raise
the Core policy. A query returns:

- `complete` when all selected facts and required capabilities are fresh and
  complete;
- `partial` when a bounded relation segment is absent, a budget is exhausted,
  or an optional capability is unavailable; the response names the boundary;
- `unsupported` when a required capability or compatible package is absent;
- `no_result` only when the declared complete candidate set has no matches.

The resolved plan and package provenance are machine-readable response
metadata. A result must never claim that a framework macro was evaluated when
its required relation capability was unavailable.

## Core, package and worker ownership

| Owner | Responsibility |
| --- | --- |
| Core | Parse/validate manifests and DSL, type-check parameters, expand declarative macros, enforce bounds, plan persistent relations, return response/provenance. |
| Package | Declare version, capability requirements, macros, command schemas and presentation-neutral result projections. |
| Project | Compose imported packages into named repository commands and bind project-specific components/prefixes. |
| Worker | Advertise and, in a future version, materialize declared language-native relation capabilities for bounded candidates. It never owns package registration or query persistence. |

## MVP boundary

The MVP proves one Spring macro and one project-local command on Spring CRUD,
selecting both Kotlin and Java annotated source declarations. It compiles only
to existing persistent relation/posting primitives. Worker-declared custom
capabilities, downloadable package distribution, negation/path traversal and
lazy dependency graph materialization are follow-up work; their absence is
represented as explicit capability or completeness state, not silently hidden.
