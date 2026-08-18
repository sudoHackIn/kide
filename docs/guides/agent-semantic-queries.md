# Agent Semantic Query Workflow

KIDE queries are bounded semantic lookups over the persistent index. Start by
indexing the workspace; a query itself does not start a language worker.

```sh
kide index .
kide query list
kide query describe spring.controllers
kide query spring.controllers
```

`list` discovers project-local commands under `.kide/queries/`. `describe`
prints the typed parameter schema. Invoke a parameterized command with a
repeatable `--param name=value` option:

```sh
kide query payments.controllers --param component=gradle:payments-api:main
```

The command emits one JSONL `SelectorRecord` per matching declaration. Its
`symbol.id` is the stable input for navigation fan-out. For example, select an
ID from the output and use it directly:

```sh
kide query spring.controllers
kide refs 'jvm:…'
kide implementations 'jvm:…'
kide callers 'jvm:…'
```

The Spring CRUD fixture is the mixed-language proof. Its project command
`spring.controllers` expands the package macro `spring.web.controller()` and
returns both Kotlin `BookController` and Java `JavaBookAuditController`.
The macro resolves the external Spring annotation through the dependency
classpath; agents do not need to materialize the Spring JAR before querying.

## Result and failure handling

The process exit status is meaningful:

| State | Meaning | Agent action |
| --- | --- | --- |
| `ok` | complete records | consume records or fan out by `symbol.id` |
| `no_result` | complete bounded posting is empty | treat as an answer, not an error |
| `stale` | matching facts are incomplete or stale | run `kide index .`, then retry |
| `unsupported` | a required capability is absent | report the missing capability; do not infer a result |
| rejected input | unknown command, macro, or typed parameter | run `query list`/`describe` and correct the invocation |

A command validates its manifest, macro name, DSL shape, and parameter types
before opening the index. Unknown or missing parameters are therefore safe to
retry and never start a worker.

## Reproducible performance check

Run the full proof, including a cold disposable JVM worker and both languages:

```sh
make e2e
```

On the current development machine the latest passing run indexed the Spring
fixture cold in about 12 seconds; warm symbol/select/navigation operations were
typically 3–16 ms. Treat those as an envelope, not a cross-machine SLO: cache,
classpath size, CPU, and first Gradle/Kotlin compilation materially affect the
cold path. Query execution remains a bounded posting lookup after indexing.
