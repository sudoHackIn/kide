# Codanna: разбор применительно к KIDE

- Статус: зафиксированный конкурентный разбор
- Проверено: 2026-08-21
- Проект: [`bartolli/codanna`](https://github.com/bartolli/codanna)
- Проверенная версия: `0.13.3`, revision `2b7bd8f`
- Лицензия: [Apache-2.0](https://github.com/bartolli/codanna/blob/2b7bd8fa4cfb06313886cd5579ba26accc746ac3/LICENSE)

## Вывод

Codanna — открытый, локальный и наиболее близкий из проверенных direct product
competitors KIDE. Это не закрытый SaaS: исходники опубликованы под Apache-2.0,
индекс хранится в repository, а local embedding backend работает без отправки
исходников наружу. Remote OpenAI-compatible embeddings являются opt-in.

Codanna объединяет две разные capability families:

1. embedding-based semantic retrieval по смыслу;
2. structural symbol graph с собственным evidence-gated relationship resolver.

Вторая часть заметно сильнее простого name matching в `ast-index` или
проверенной модели `codebase-memory-mcp`: Codanna хранит symbol IDs, учитывает
imports/modules/containing types/receivers и предпочитает оставить edge
unresolved, если выбор нельзя обосновать.

Но semantic authority все равно строится поверх Tree-sitter evidence и
собственного resolver, а не Kotlin K2/javac type checking. Java/Kotlin project
providers разбирают source roots и package layout; они не материализуют
полноценный object universe из JVM classpath/JAR dependencies. Поэтому Codanna
является главным benchmark продуктовой поверхности KIDE, но не заменяет
compiler/IDE correctness baseline.

Ключевой вывод глубокого сравнения:

> Codanna оптимизирует получение агентом **быстрого и достаточно обоснованного
> контекста**. KIDE должен оптимизировать получение **адресуемых,
> компиляторно подтвержденных фактов**, включая JDK и binary dependencies.

Это реальная конкурентная граница, а не основание считать Codanna слабой.
Сегодня Codanna значительно сильнее как готовый agent product; KIDE сильнее по
целевой модели semantic authority и dependency knowledge, но часть этого
преимущества еще требует исправлений и benchmark.

## Что означает `semantic` в Codanna

### Semantic retrieval

`semantic_search_docs` и `semantic_search_with_context` создают embeddings для
symbol text и ищут похожие vectors. По умолчанию используется локальный
FastEmbed-compatible model; vector index сохраняется на disk.

Это настоящий поиск по смыслу:

```text
"where do we validate authentication tokens"
                  ↓
embedding similarity
                  ↓
candidate symbols + signatures + docs
```

Он помогает агенту найти неизвестное имя, но similarity score не доказывает
definition/reference/call relationship.

### Program relationships

После structural parsing Codanna пытается разрешить calls, inheritance, uses и
definitions в конкретные symbol IDs. Resolver использует language-specific
evidence:

- local и imported identities;
- module/package path;
- containing class;
- receiver name и inferred receiver evidence;
- visibility и file-private restrictions;
- inheritance;
- exact call site.

При ambiguity новые версии намеренно fail closed. Changelog приводит случаи,
где число resolved edges уменьшилось после удаления guessed matches. Это
правильный product principle и важный ориентир для KIDE.

См. [README](https://github.com/bartolli/codanna/blob/2b7bd8fa4cfb06313886cd5579ba26accc746ac3/README.md)
и [resolution changelog](https://github.com/bartolli/codanna/blob/2b7bd8fa4cfb06313886cd5579ba26accc746ac3/CHANGELOG.md#0130---2026-08-01).

## Архитектурная поверхность

Codanna предоставляет:

- persistent local symbol/index storage;
- full-text и vector indexes;
- CLI и native MCP server;
- stdio, HTTP и HTTPS transports;
- watch mode и incremental updates;
- symbol search и reusable symbol IDs;
- callers/callees, implementations и impact analysis;
- fused `semantic_search_with_context`, который сразу добавляет relationships;
- document collections/RAG;
- 15 языков, включая Java и Kotlin.

Особенно важен fused query. Вместо пяти последовательных tool calls агент
получает candidate, signature, documentation, callers, callees и impact context
одним bounded ответом. Это сильный ориентир для MCP design KIDE.

## Насколько точны Java и Kotlin

Java и Kotlin parsers используют Tree-sitter grammars. Language-specific
resolution значительно сокращает очевидные false edges, но не воспроизводит
полную compiler semantics:

- parser не выполняет overload applicability и generic substitution;
- receiver evidence не эквивалентен compiler type;
- implicit receivers, smart casts и complex Kotlin resolution остаются
  трудными;
- dynamic/incomplete evidence часто приводит к `unresolved`;
- внешний symbol, которого нет в source index, не становится автоматически
  materialized declaration.

Project providers для Maven/Gradle извлекают source roots и package/module
layout. Это необходимо для cross-file resolution, но не равно чтению
resolved build classpath и индексированию каждого class/member из JAR.

Показательный принцип Codanna: external Kotlin imports не должны участвовать в
resolution, если соответствующей internal identity нет. Это повышает precision,
но одновременно подтверждает отличие от глобального dependency catalog KIDE.

## Сопоставление с KIDE

| Возможность | Codanna | KIDE |
| --- | --- | --- |
| Лицензия/развертывание | Apache-2.0, local-first | локальный проект; публичное license оформление KIDE требует отдельной проверки |
| Parser authority | Tree-sitter | K2/javac/bytecode workers |
| Natural-language search | встроенные embeddings | optional future retrieval layer |
| Relationship resolution | evidence-gated custom resolver | K2/javac-resolved source facts; ASM binary declarations |
| Ambiguity | candidates/unresolved вместо случайного pick | fact-level quality model; некоторые external targets пока unresolved |
| Persistent index | project-local indexes | source store + reusable dependency artifacts |
| External JVM objects | не materialized как полный classpath universe | частично реализованный global dependency catalog |
| MCP | зрелый native product | P1 epic |
| Languages | 15 | Kotlin/JVM-first |
| Transformations | retrieval/impact focus | planned exact semantic transformations |

## Глубокая граница semantic authority

Codanna строит relationship примерно так:

```text
Tree-sitter fact
  → language-specific scope/import/receiver evidence
  → candidate filtering и inheritance walk
  → exactly-one survivor
  → resolved edge или unresolved
```

Современный resolver отдельно обрабатывает `this`/`self`, `super`, static и
typed receivers, visibility, imports и inherited members. При неизвестном типе
receiver или нескольких равноправных candidates он часто отказывается
создавать edge. Поэтому наличие Tree-sitter само по себе не означает, что
результат является простым AST guess.

Но resolver доказывает более слабое утверждение:

> Среди известных source candidates один symbol лучше всего подтверждается
> доступными syntactic и locally inferred evidence.

KIDE для source может хранить другое утверждение:

> Этот declaration/callable выбрал K2 или javac при конкретном classpath,
> compiler version и analysis options.

Разница становится существенной на overload applicability, Kotlin extensions,
implicit receivers, type aliases, generic calls, overrides и Java/Kotlin
interop. В текущем KIDE получение K2 target идет из
`FirResolvedNamedReference.resolvedSymbol`, то есть после compiler resolution,
а не через повторное сопоставление имени.

## Что в KIDE уже доказано кодом и тестами

- K2 fixture различает точный source overload.
- K2 разрешает extension и implicit-receiver calls.
- Записываются direct hierarchy и exact override edges.
- Generic call и constructor через `typealias` связываются с declaration.
- `javac` используется как semantic authority для Java source.
- ASM различает binary overload declarations по JVM descriptor.
- На Spring/Gradle fixture K2 видит external classpath и сохраняет по крайней
  мере однозначные Kotlin → Java dependency references.
- Dependency blobs имеют content/context/provenance-aware identity, лежат вне
  project index и поддерживают bounded section reads.
- Canonical facts различают precision, freshness, completeness и provenance;
  applications и types являются first-class records.

Именно сочетание compiler facts, global dependency objects и cross-workspace
reuse является наиболее сильной стороной KIDE. Ни embeddings, ни обычный
project-local call graph сами по себе не создают этот moat.

Проверяемые места реализации:

- [canonical quality и graph records](../../crates/kide-core/src/canonical.rs);
- [K2 FIR collector](../../workers/kotlin-jvm/src/main/kotlin/dev/kide/worker/KideFirCollector.kt);
- [ASM dependency extraction и target mapping](../../workers/kotlin-jvm/src/main/kotlin/dev/kide/worker/JvmBytecodeExtractor.kt);
- [shared artifact cache](../../crates/kide-core/src/artifact_cache.rs) и
  [bounded artifact queries](../../crates/kide-core/src/artifact_query.rs);
- [K2 structural fixtures](../../workers/kotlin-jvm/src/test/kotlin/dev/kide/worker/KotlinStructuralBatchTest.kt) и
  [Spring/classpath integration fixtures](../../workers/kotlin-jvm/src/test/kotlin/dev/kide/worker/K2SpringCrudIntegrationTest.kt).

## Что пока не является доказанным преимуществом KIDE

### External overloaded calls

Bytecode extractor уже создает разные IDs для JVM overload declarations, но
текущий K2-to-bytecode mapper сопоставляет callable по owner/member и намеренно
не выбирает target при нескольких overloads. Нужен deterministic перевод
compiler callable key в JVM owner/name/descriptor.

### Cross-artifact identity

При чтении одного JAR внешний superclass или annotation target пока может
получить ID, содержащий hash текущего artifact, а не artifact, где target
объявлен. До введения portable locator и отдельной materialization-resolution
фазы нельзя объявлять полностью точными hierarchy/annotation queries через
несколько независимо построенных JAR blobs.

### Operational cost

Compiler/build import и полный dependency warmup заметно тяжелее Tree-sitter
pipeline Codanna. Content-addressed reuse должен окупать эту стоимость на
повторных workspace, но это необходимо доказать latency, storage и cache-hit
benchmark, а не только архитектурой.

### Agent product surface

В KIDE пока нет зрелого MCP, watch UX, embedding discovery, document RAG и
Codanna-подобного fused context tool. Поэтому Codanna сегодня выигрывает
практический сценарий «подключить агента и быстро исследовать незнакомый repo».

## Где Codanna сильнее KIDE сегодня

- Готовый MCP и CLI product.
- Реальный embedding semantic search.
- Fused context tools, оптимизированные под agent workflow.
- Watch mode и incremental lifecycle.
- Многоязычность.
- Явная работа с ambiguous symbol names.
- Публичные performance claims и воспроизводимые benchmark descriptions.
- Быстро развивающийся resolution layer с измерением precision trade-offs.

Codanna нельзя списывать как «просто Tree-sitter». Ее resolver реализует
разумный middle ground между syntax graph и compiler index и может оказаться
достаточно точным для большинства повседневных agent navigation tasks.

Особенно важно различать два пользовательских запроса:

| Задача | Текущий лидер |
| --- | --- |
| «Где находится логика retry/authentication?» | Codanna semantic retrieval |
| Быстро получить symbol + callers + callees + impact | Codanna MCP product |
| Какой source overload/extension выбрал Kotlin | KIDE/K2 на покрытых fixtures |
| Найти объекты JDK/JAR как часть project universe | целевая специализация KIDE |
| Повторно использовать semantic artifact одной dependency | архитектура KIDE |
| Точно пройти hierarchy через несколько JAR | еще не доказано KIDE |

## Где должна отличаться KIDE

KIDE не должен соревноваться с Codanna общим обещанием «semantic code search».
Защищаемое различие конкретнее:

- compiler-selected callable и declaration identities;
- overloads и JVM descriptors;
- Kotlin/Java cross-language semantics;
- source + JDK + binary dependency search space;
- content-addressed reuse уже построенных dependency indexes;
- per-answer freshness, completeness, precision и provenance;
- transformations, для которых wrong edge недопустим.

Embedding retrieval при необходимости можно добавить поверх KIDE. Обратное
преобразование — получить compiler truth из уже построенного heuristic graph —
невозможно без нового semantic extraction layer.

## Что стоит перенять

1. `semantic_search_with_context`: discovery и graph context одним bounded
   запросом.
2. Stable symbol IDs в ответах, чтобы последующие tools не повторяли name
   resolution.
3. Refuse-and-list при ambiguity.
4. Evidence-gated resolution как fallback policy даже для partial backends.
5. Exact call-site evidence рядом с edge.
6. Watch/incremental UX и одинаковую policy для CLI и MCP.
7. Раздельный status embedding index и structural index.
8. Benchmark не только latency, но и изменение precision/recall после каждого
   resolver improvement.
9. Разделить discovery handle и exact identity: semantic match должен сначала
   вернуть candidates, после чего graph queries работают по стабильному ID.
10. Публиковать operational показатели на реальном hardware и corpus, включая
    indexing, embedding и warm-query cost отдельно.

## Что не стоит перенимать

- Называть heuristic relationship compiler-exact только потому, что у него есть
  target symbol ID.
- Подменять classpath model разбором Maven/Gradle source roots.
- Делать embeddings обязательным runtime для exact queries.
- Расширять языки раньше доказательства Kotlin/JVM correctness.
- Смешивать vector similarity и exact relationship confidence.

## Роль в стратегии и benchmark

Codanna должна использоваться как:

- главный прямой benchmark local agent code-intelligence product;
- baseline natural-language discovery;
- baseline evidence-gated structural resolution;
- образец MCP composition и ambiguity UX;
- обязательный участник JVM differential corpus.

Она не является ground truth для JVM. Ground truth по-прежнему должен
формироваться compiler/IDE authorities: K2, javac, IntelliJ и, с оговорками,
`scip-java`.

Приоритетный вывод для roadmap:

1. **P0:** замкнуть существующий fast incremental source path в проверенный
   whole-workspace freshness contract и доказать dependency cache reuse.
2. **P0:** закончить portable cross-artifact identity и external overload
   mapping; без этого главный dependency claim неполон.
3. **P1:** начать CLI-based agent dogfooding, добавить bounded fused context и
   расширять global dependency catalog по реальным задачам.
4. **P1:** выпустить read-only MCP только после стабилизации CLI/Core contract.
5. **P2:** построить comparative corpus из dogfood traces и затем сравнить KIDE
   с Codanna, `scip-java`, IntelliJ/compiler и structural baselines.
6. **P2:** добавить lexical/vector discovery как отдельный optional index;
   similarity не должна менять exact relationship graph.
