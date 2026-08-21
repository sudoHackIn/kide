# SCIP, `scip-java` и Sourcegraph: разбор применительно к KIDE

- Статус: зафиксированный конкурентный разбор
- Проверено: 2026-08-21
- SCIP: [`scip-code/scip`](https://github.com/scip-code/scip)
- JVM indexer: [`scip-code/scip-java`](https://github.com/scip-code/scip-java)
- Платформа: [Sourcegraph Code Navigation](https://sourcegraph.com/docs/code-navigation)

## Вывод

SCIP, `scip-java` и Sourcegraph — не одно и то же:

```text
javac / kotlinc / build model
             ↓
         scip-java
             ↓
        index.scip
             ↓
 Sourcegraph storage/query/UI/API
```

- **SCIP** — language-neutral Protobuf format для предвычисленных symbol и
  navigation facts.
- **`scip-java`** — JVM indexer, производящий SCIP из реальной сборки.
- **Sourcegraph** — серверная платформа, которая хранит, компонует и запрашивает
  загруженные индексы.

SCIP следует рассматривать как полезный interoperability format, а не как
внутреннюю каноническую модель KIDE. `scip-java` при этом является прямым
конкурентом и correctness baseline для JVM extraction, особенно Java.

## SCIP не является LSP

LSP — протокол взаимодействия с живым language server, ориентированный главным
образом на document/position requests и synchronized editor state.

SCIP — статический snapshot. Его можно построить в CI или batch worker,
загрузить один раз и выполнять navigation без работающего compiler или language
server. Поэтому сам факт существования SCIP подтверждает основную архитектурную
предпосылку KIDE: compiler frontend не обязан оставаться runtime каждого
последующего запроса.

SCIP, однако, не является query engine, database или MCP server. Он не задает
политику incremental invalidation, storage layout, latency, completeness или
agent tools.

## Что содержится в SCIP

Актуальная схема определена в
[`scip.proto`](https://github.com/scip-code/scip/blob/main/scip.proto).

Основные records:

- `Index`: metadata, documents и optional external symbols;
- `Document`: path, language, occurrences, defined symbols и optional text;
- `SymbolInformation`: portable symbol, documentation, kind, display name,
  signature и relationships;
- `Occurrence`: source range, symbol roles, syntax kind, diagnostics,
  documentation override и enclosing AST range;
- `Relationship`: reference-equivalence, implementation, type-definition и
  alternate definition;
- `Package`: manager, name и version.

SCIP symbol grammar стандартизует package identity и descriptor chain. Method
descriptor имеет disambiguator, позволяющий различать overloads.

SCIP не содержит first-class normalized call graph, build graph, полноценную
type model или per-fact quality state. Вызов представлен как occurrence,
ссылающийся на callable; call hierarchy может дополнительно использовать
enclosing ranges.

## Что делает `scip-java`

Для Java `scip-java` реализован как plugin `javac`, выполняющийся внутри
обычной Gradle/Maven/Bazel compilation. Это дает ему доступ к compiler-resolved
symbols, classpath, options и annotation processors. Partial per-file shards
после compilation объединяются в `index.scip`.

Текущее заявленное состояние:

- Java поддерживается для актуальных LTS JDK 17, 21 и 25;
- automatic Gradle и Maven integration поддерживает Java;
- Kotlin automatic integration поддерживается для Gradle;
- Kotlin через Maven и Bazel автоматически не поддерживается;
- Android integration не поддерживается;
- Kotlin support официально характеризуется как менее зрелый, чем Java;
- cross-repository navigation требует корректной dependency/package metadata.

См. [getting started](https://github.com/scip-code/scip-java/blob/main/docs/getting-started.md)
и [design](https://github.com/scip-code/scip-java/blob/main/docs/design.md).

## `scip-java` против нашего JVM extraction

### Java

Сегодня `scip-java` следует считать более зрелым Java indexer:

- compiler plugin работает в реальной compilation;
- богаче occurrences и source navigation;
- есть portable package-aware symbols;
- решены многие language-version и build integration cases;
- есть сложившийся snapshot-testing workflow.

KIDE уже сохраняет JVM descriptor в member ID и тем самым способен различать
overloads dependency bytecode. Но текущий dependency materializer переносит в
blob главным образом declarations и hierarchy, а не полный набор source facts.

### Kotlin

Здесь лидерство не установлено. `scip-java` имеет работающую Kotlin integration,
но сам проект отмечает ее меньшую зрелость. KIDE использует K2/FIR как native
semantic authority и потенциально способен лучше покрыть extensions, implicit
receivers, smart casts и Kotlin/Java interop. Это пока гипотеза, требующая
differential benchmark.

### Практическое решение

Не нужно переписывать возможности `scip-java` вслепую. Его следует оценить в
трех ролях:

1. correctness baseline для Java;
2. optional disposable producer/backend для KIDE;
3. источник или приемник SCIP при interoperability.

Moat KIDE должен находиться не только в extraction, а в persistent composition,
quality contract, query engine, dependency reuse и agent-facing API.

## SCIP и KIDE blobs

Это разные по границе артефакты:

```text
SCIP:
repository revision → один portable workspace navigation index

KIDE:
ProjectManifest
  + SQLite source facts
  + N immutable content-addressed dependency blobs
```

Эквивалентом целого `index.scip` является собранное представление KIDE, а не
один dependency blob.

| Область | SCIP | KIDE artifact blob |
| --- | --- | --- |
| Гранулярность | workspace/revision | resolved dependency artifact |
| Source unit | `Document` | `GraphSnapshot` + `ArtifactSourceUnit` |
| Symbol identity | portable package + descriptors | opaque ID/backend key; JAR hash + JVM descriptor |
| Symbol metadata | богатый enum, docs, linked signature | kind/name/qname/plain signature/owner/modifiers |
| Occurrence | roles, syntax, docs, diagnostics, enclosing range | explicit kind, target, enclosing symbol, type ID, quality |
| References | occurrences | explicit edges |
| Calls | не first-class | explicit selected target + optional caller |
| Hierarchy | relationship flags | explicit subtype → supertype edge |
| Types | hover/type-definition oriented | explicit, пока неглубокий `TypeRecord` |
| Quality | доверие к indexer/revision | precision, freshness, completeness, provenance |
| External deps | package identity, external symbols | отдельные cached JAR blobs |
| Physical access | streaming monolithic Protobuf | TOC, checksums, postings, bounded detail blocks |

Наша схема находится в [`artifact.proto`](../../protocol/kide/artifact.proto),
физический layout — в
[`artifact_blob_layout.rs`](../../crates/kide-core/src/artifact_blob_layout.rs).

## Что в KIDE schema и что реально записывается

`GraphArtifact` предусматривает symbols, occurrences, references, calls,
hierarchy, types и diagnostics. Но текущий dependency materializer копирует в
blob source unit, provenance, symbols, hierarchy и completeness. Bytecode
extractor оставляет occurrences/references/calls/types/diagnostics пустыми и
честно отмечает snapshot как `partial`.

См. [`ArtifactMaterializer.kt`](../../workers/kotlin-jvm/src/main/kotlin/dev/kide/worker/ArtifactMaterializer.kt)
и [`JvmBytecodeExtractor.kt`](../../workers/kotlin-jvm/src/main/kotlin/dev/kide/worker/JvmBytecodeExtractor.kt).

Есть также schema gap: канонический `ApplicationFact` поддерживает
структурированные аргументы annotations/decorators, но dependency artifact
schema его пока не хранит. В blob остаются только `applied_symbol_ids`.

## Критический gap: cross-artifact identity

SCIP package identity рассчитана на композицию независимо построенных индексов.

KIDE надежно идентифицирует declaration внутри JAR через content hash и JVM
descriptor. Однако текущий bytecode extractor строит ID внешнего superclass или
annotation с hash анализируемого JAR. Если declaration находится в другом JAR,
такой ID не совпадает с ID реального владельца. Artifact query позднее сравнивает
hierarchy по точному `SymbolId`; отдельной переклейки по qualified name сейчас
нет.

Следствие: внутри одного artifact identity точна, но cross-artifact hierarchy и
annotation edges могут оставаться dangling. Перед обещанием глобального поиска
по dependencies это нужно исправить.

## Что стоит перенять из SCIP

1. Portable external symbol key на основе ecosystem/package/version/descriptors.
2. `ExternalSymbolStub` для еще не материализованной зависимости.
3. Documentation и signatures с occurrences внутри типов.
4. Occurrence roles: read/write/import/generated/test/forward definition.
5. Enclosing expression/declaration ranges.
6. Явные relationship kinds для overrides, type definitions и definition aliases.
7. Diagnostic source и tags.
8. Deterministic snapshot fixtures для проверки indexer correctness.

Portable key должен быть alias/interchange identity рядом с внутренним
`SymbolId`, а не заменять content-aware canonical identity KIDE.

## Что не стоит переносить во внутреннюю модель

- Монолитный workspace index вместо секционного random access.
- Отсутствие per-fact freshness/completeness/precision.
- Только line/column ranges как внутреннее хранение; byte ranges KIDE компактнее
  и однозначно привязаны к content snapshot.
- Navigation-only границу как предел модели: KIDE нужны first-class calls,
  types, build context и declarative application facts.

## Роль Sourcegraph

Sourcegraph пересекается с KIDE по precise definition/references/implementations
и cross-repository navigation. Поэтому это смежный продуктовый конкурент, а не
просто нейтральный формат.

Но его основная граница — централизованная platform для code search/navigation.
KIDE ориентируется на локальный/headless agent runtime, richer semantic graph и
dependency artifact reuse. SCIP export позволяет превратить Sourcegraph из
только конкурента в distribution channel.
