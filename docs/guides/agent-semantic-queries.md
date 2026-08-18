# Agent Semantic Query Workflow

KIDE queries are bounded semantic lookups over the persistent index. Start by
indexing the workspace. Plain Java/Kotlin relation queries remain worker-free;
a package with required compatibility may perform a disposable handshake, and
only an explicit `using capability ...` step sends a bounded query request.

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

Framework packages compose without adding framework vocabulary to Core. The
same fixture installs `jakarta.persistence` beside `spring.web`; its independent
project command uses the same generic resolved-annotation posting:

```sh
kide query describe persistence.entities
kide query persistence.entities
```

The JSONL plan identifies only the package selected by that command, including
its version and manifest digest. Installing another package therefore does not
change the resolution or provenance of existing Spring commands.

## Spring declaration inventory

The fixture also installs `spring.core` for Spring Framework annotations and
`spring.boot` for Spring Boot annotations. Both provide direct, bounded
declaration lookups: they answer *where an annotation is declared*, not whether
Spring creates or selects a bean at runtime.

| Command | Direct annotation selected | Declaration kinds |
| --- | --- | --- |
| `spring.core.components` | `@Component` | classes |
| `spring.core.services` | `@Service` | classes |
| `spring.core.repositories` | `@Repository` | indexed annotation owners |
| `spring.core.configurations` | `@Configuration` | classes |
| `spring.core.beans` | `@Bean` | indexed annotation owners (normally methods/functions) |
| `spring.core.qualifiers` | `@Qualifier` | any indexed annotation owner |
| `spring.core.primaries` | `@Primary` | indexed annotation owners |
| `spring.boot.autoconfigurations` | `@AutoConfiguration` | classes |

For example:

```sh
kide query spring.core.components
kide query spring.core.beans
kide query spring.boot.autoconfigurations
```

`spring.boot.autoconfigurations` deliberately selects
`org.springframework.boot.autoconfigure.AutoConfiguration`. It does **not**
select `@EnableAutoConfiguration`: that annotation enables the discovery
mechanism in an application and is not itself an auto-configuration
declaration.

These commands do not expand meta-annotations, compute component scanning,
evaluate profiles or conditions, inspect generated/auto-registered beans, or
resolve an injection point. A future bean-environment query may use these
declarations as evidence, but it must report any runtime-dependent case as
partial rather than infer activation or candidate selection.

Queries may also start from the persisted direct hierarchy posting. Use
`subtype_of(...)` or its equivalent `implements(...)` spelling; both are
direct-only and require the same `symbol-id` or `qualified-symbol` values as
`applies(...)`:

```text
command spring.repositories() {
  from subtype_of(qualified-symbol("org.springframework.data.jpa.repository.JpaRepository"))
  where kind == interface
  return symbol
  limit 100
}
```

Qualified dependency interfaces resolve through the artifact catalog without
materializing their declarations. The resulting implementations are ordinary
JSONL `SelectorRecord`s and can be piped into navigation commands.

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
