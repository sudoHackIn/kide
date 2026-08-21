# Позиционирование и приоритеты KIDE

- Статус: рабочее стратегическое решение
- Зафиксировано: 2026-08-21
- Основание: отдельные разборы
  [`codebase-memory-mcp`](codebase-memory-mcp.md),
  [`Claude-ast-index-search`](claude-ast-index-search.md),
  [Codanna](codanna.md) и
  [SCIP / `scip-java` / Sourcegraph](scip-sourcegraph-scip-java.md)

## Решение

Рабочее позиционирование KIDE:

> **KIDE — headless persistent compiler-derived semantic index для coding
> agents, MCP и CI, с глобальными запросами по project и dependency objects,
> явным quality contract и переиспользованием content-addressed dependency
> artifacts без постоянно работающего language server.**

Короткая формула:

```text
compiler authority
× persistent normalized graph
× reusable dependency knowledge
× agent-native bounded queries
```

KIDE не должен позиционироваться как еще один multi-language code graph,
MCP-wrapper над LSP, замена Sourcegraph или новый universal index format.

Это не заявление об архитектурной новизне persistent compiler indexes вообще.
SCIP/Sourcegraph и Kythe уже доказали, что compiler-derived knowledge можно
сохранять и обслуживать без resident LSP. Более узкая ставка KIDE — соединить
compiler-grade JVM semantics, локальный agent product, глобальный поиск по
source/JDK/dependencies и cross-workspace reuse dependency artifacts.

## Карта продуктов и решений

| Система | Роль относительно KIDE | На что ориентироваться | Что не копировать |
| --- | --- | --- | --- |
| `codebase-memory-mcp` | конкурент за agent UX, не correctness authority | onboarding, MCP ergonomics, bounded graph context, agent-efficiency | name-based semantic identity и silent approximate edges |
| `ast-index` | прямой конкурент CLI/MCP product surface; structural baseline | command vocabulary, incremental UX, thin MCP adapter, компактный output | строковые refs/parents и grep callers под exact-looking именами |
| Codanna | главный открытый direct product competitor | fused semantic+graph context, stable IDs, refuse-on-ambiguity, watch mode, public benchmarks | считать evidence-gated Tree-sitter resolver compiler authority |
| Serena | конкурент agent workflow; LSP/JetBrains adapter | компактные symbol tools, editing workflow, agent integration | зависимость core architecture от resident LSP/IDE |
| LSP | optional frontend protocol | editor compatibility при наличии спроса | document-synchronized runtime как единственный semantic store |
| SCIP | interoperability format | export/import, portable symbols, occurrences, docs, relationships | использование navigation schema как канонического graph model |
| `scip-java` | прямой JVM indexer benchmark | Java correctness, compiler-plugin integration, fixtures | предположение, что SCIP output решает storage/query/agent product |
| Sourcegraph | смежный navigation/search competitor и channel | cross-repo composition, upload ecosystem, scalable serving | enterprise platform scope как MVP KIDE |
| Kythe | архитектурный prior art, не agent-product competitor | compilation units, extractor/indexer/serving separation, durable xrefs | тяжелую infrastructure-first упаковку и broad platform scope |
| IntelliJ | functional gold standard | global project+library objects, JVM correctness, hierarchy/refactoring | GUI IDE runtime и закрытая stateful architecture |
| Augment Context Engine | закрытый outcome competitor | доказательство спроса на indexed context через MCP/SDK | недоказуемые claims о внутренней точности и hosted scope |

## Сводное сравнение текущих сильных сторон

Здесь важно сравнивать не абстрактные vision, а сегодняшние capability classes:

| Capability | Сильнейший ориентир сейчас | Позиция KIDE |
| --- | --- | --- |
| Natural-language discovery | Codanna | отдельного vector retrieval пока нет |
| Agent MCP composition | Codanna, Serena | CLI primitives есть; MCP и fused context еще не готовы |
| Kotlin/Java source identity | K2/javac, IntelliJ | compiler-selected facts уже проверены на focused fixtures |
| Быстрый multi-language structural index | Codanna, `ast-index`, `codebase-memory-mcp` | сознательно не основной scope |
| Portable navigation exchange | SCIP | KIDE должен экспортировать/импортировать, не заменять формат |
| Java compiler index baseline | `scip-java` | differential oracle и возможный backend/input |
| Global project+library objects | IntelliJ | целевой headless аналог; dependency catalog частично реализован |
| Reuse одного dependency index между workspace | KIDE architecture | cache существует, product/economic advantage еще не доказан |
| Fact-level quality и provenance | KIDE model | уже есть в canonical records, требуется сохранить во всех APIs |
| Exact cross-JAR composition | IntelliJ/compiler build universe | критический незакрытый gap KIDE |

Главная конкурентная асимметрия:

```text
Codanna: discovery → useful context → evidence-gated project graph
KIDE:    compiler/build facts → persistent exact graph → dependency reuse
SCIP:    portable navigation document exchanged between producers/consumers
Serena:  agent workflow over a live LSP/IDE semantic backend
```

KIDE не должен пытаться победить всех по одному измерению. Его защищаемая
комбинация — compiler authority, global dependency universe, persistence/reuse
и explicit quality contract. Codanna остается продуктовым эталоном того, как
эту глубину сделать дешевой для агента.

## Границы competitive set

Для принятия ближайших решений достаточно трех групп.

### Прямые agent-product competitors

- **Codanna** — самый сильный открытый hybrid competitor: embeddings,
  persistent graph, evidence-gated resolution, CLI и MCP.
- **`ast-index`** — самый близкий CLI/MCP form factor, но существенно более
  слабый semantic contract.
- **`codebase-memory-mcp`** — сильный agent graph и onboarding benchmark.
- **Serena** — workflow competitor, использующий LSP или JetBrains backend.

### Semantic infrastructure и correctness authorities

- **SCIP** — interoperability format.
- **`scip-java`** — JVM indexer и differential baseline.
- **Kythe** — prior art persistent compiler/index/serving pipeline.
- **IntelliJ** — functional ground truth project+library semantics.

### Outcome и distribution competitors

- Sourcegraph и Augment продают конечный outcome: быстрый repository context,
  navigation и agent retrieval.
- Их platform scope не должен становиться scope KIDE MVP.

GitNexus, Probe, CodeGraphContext, CodeQL, Joern и многочисленные новые
Tree-sitter MCP graphs остаются watchlist. Отдельный глубокий разбор каждого не
нужен до появления отличающей capability: Codanna и `ast-index` уже достаточно
хорошо представляют local structural/hybrid agent-index class, а CodeQL/Joern
решают другой batch-analysis use case.

## Два значения semantic search

В позиционировании нельзя смешивать:

1. **Semantic retrieval** — embeddings отвечают, какой код похож по смыслу на
   natural-language query.
2. **Program semantics** — compiler отвечает, к какой declaration относится
   occurrence, какой overload выбран и какой type имеет expression.

Codanna сильна в первом и строит разумный heuristic/evidence-gated слой для
второго. KIDE специализируется на втором. В будущем embedding retrieval может
находить candidate `SymbolId`, после чего exact navigation обязана выполняться
по compiler-derived graph.

После глубокого разбора Codanna это различие становится обязательной частью
терминологии KIDE:

- `search/discover` может быть lexical, vector или approximate и возвращает
  ranked candidates;
- `resolve/navigate/query` работает по exact identity и не наследует similarity
  score как semantic confidence;
- agent-facing fused tool может объединять оба этапа, но ответ обязан явно
  показывать границу retrieval и compiler-derived facts.

## Почему LSP не закрывает целевой use case

LSP определяет `workspace/symbol`, но не гарантирует:

- включение JDK и dependency libraries;
- полноту результатов;
- package/artifact/version scope;
- запросы по annotations, signatures, supertypes и semantic relations;
- persistence и reuse между workspace;
- freshness/completeness/precision ответа.

Конкретный server может реализовать часть этих возможностей как extension или
внутреннюю функцию. Но агент не получает переносимого semantic catalog
contract.

JetBrains подтверждает ценность другого подхода: project analysis индексирует
source code, SDK и libraries, после чего classes/methods/fields зависимостей
становятся частью глобального search space. KIDE строит headless и agent-facing
вариант этой идеи.

Целевые запросы:

```text
найти класс/метод/поле во всех project и dependency artifacts
найти все overloads с точными signatures
найти реализации interface, включая dependency JARs
найти declarations с resolved annotation
найти callers/references точного SymbolId
ограничить поиск component, Maven coordinate, version или scope
```

## Что подтверждено

### Кодом KIDE

- Rust persistent store и normalized semantic records существуют.
- JVM worker отделен от Core и может быть disposable.
- Dependency artifacts имеют content/context/provenance-aware cache identity.
- Blobs immutable, section-addressable и имеют per-section checksums.
- Symbol details и hierarchy можно читать без декодирования полного graph.
- Модель различает precision, freshness, completeness и provenance.
- First-class records существуют для references, calls, hierarchy и types.
- K2 collector берет target из resolved FIR symbol и на focused fixtures
  различает source overloads, extensions, implicit receivers, overrides,
  generics и constructor через typealias.
- Java source extraction использует `javac` analysis, а не только AST grammar.
- ASM worker различает binary overload declarations по JVM descriptor.
- На Spring/Gradle fixture K2 видит реальный classpath и сохраняет однозначные
  Kotlin → external Java symbol references.

### Текущие implementation gaps

- K2-to-bytecode mapping пока намеренно оставляет overloaded external callable
  unresolved: Kotlin callable key еще не переводится в exact JVM descriptor.
- External superclass/annotation identity внутри одного JAR blob пока может
  быть построена с hash анализируемого artifact; для независимой композиции
  нужен portable external locator и последующее связывание с owner artifact.
- Bytecode snapshots являются `partial`: declarations, signatures, direct
  hierarchy и annotations есть, но binary call bodies/occurrences/types не
  заявлены как complete.
- MCP, watch lifecycle, fused agent context и vector discovery еще не являются
  готовой product surface.
- Cold dependency warmup и размер blobs пока значительно дороже structural
  indexers; reuse должен быть доказан измерениями на нескольких workspace.

### Экспериментом

- Structural/name-based graph `codebase-memory-mcp` быстро строится и удобен для
  agent context.
- На JVM/Rust mixed repository были обнаружены false call edges.
- Java/Kotlin overload fixture показала схлопывание overload identity.
- В `ast-index` references и inheritance parents хранятся строками без target
  symbol IDs; `callers` реализован grep-поиском, а Kotlin/Java references
  извлекаются generic lexical rules.
- Codanna подтверждена как Apache-2.0 local product с persistent embeddings,
  MCP и evidence-gated resolver; при отсутствии достаточных evidence resolver
  предпочитает unresolved случайному same-name edge.
- Глубокая проверка Codanna показала отдельные paths для typed/static receivers,
  `this`/`self`, `super`, imports, visibility и inheritance. Поэтому она является
  сильным hybrid baseline, а не только Tree-sitter/name-matching baseline.

### Внешними precedents

- SCIP и Sourcegraph доказывают жизнеспособность precomputed navigation indexes
  без resident LSP.
- `scip-java` доказывает практичность compiler plugin indexing внутри build.
- Kythe доказывает жизнеспособность разделения compilation extraction,
  compiler indexing, durable graph storage и xref serving.
- IntelliJ доказывает ценность глобального object search по project и libraries.

## Что пока является гипотезой

- KIDE точнее `scip-java` на Kotlin/K2 edge cases.
- KIDE быстрее LSP/IDE на warm agent queries при сопоставимой correctness.
- Dependency blobs дадут существенный cross-workspace cache hit rate.
- Глобальные dependency queries заметно сократят agent tokens и tool calls.
- Пользователи предпочтут отдельный local semantic service существующему IDE.
- Compiler correctness окажется достаточно ценной для agent tasks, чтобы
  компенсировать меньшую ширину языков относительно Codanna и `ast-index`.
- Safe transformations смогут работать без resident IDE с приемлемой полнотой.
- Portable external locators и JVM descriptors смогут надежно скомпозировать
  independently built blobs без глобальной переиндексации.

Эти утверждения нельзя использовать как доказанные преимущества до появления
воспроизводимого benchmark.

## Product principles

1. Неправильный exact answer хуже `partial` или `unsupported`.
2. Compiler-native facts и approximate retrieval никогда не смешиваются молча.
3. Один вычисленный dependency artifact должен переиспользоваться максимально
   широко при точном совпадении identity.
4. Core владеет persistent model; compiler/build workers остаются disposable.
5. MCP, CLI, SCIP и будущий LSP — adapters над одним query contract.
6. Сначала глубина Kotlin/JVM, потом количество языков.
7. Correctness публикуется рядом с performance, не отдельно от нее.

## Приоритеты

```text
P0  замкнуть dogfood-ready freshness/incrementality и exact dependency composition
P1  начать agent dogfooding, расширить global catalog и стабилизировать MCP contract
P2  сравнивать после dogfooding; затем interoperability, discovery и transformations
P3  распределение artifacts и новые frontends/languages
```

Порядок внутри уровней также важен:

| Очередь | Решение | Почему сейчас |
| ---: | --- | --- |
| 1 | Incremental dependency cache + whole-workspace freshness closure | существующий fast source path нужно распространить на build/dependencies и все queries |
| 2 | Portable cross-artifact identity + external overload mapping | без этого главный dependency claim семантически неполон |
| 3 | CLI-based agent dogfooding + bounded context | реальная эксплуатация должна формировать следующий backlog |
| 4 | Global source/JDK/dependency catalog | расширяется по реальным agent tasks, а не в вакууме |
| 5 | Read-only MCP над стабильным contract | MCP не должен фиксировать преждевременную форму ответов |
| 6 | Competitive benchmark из dogfood traces | сравнение проверяет уже работающий продукт и реальные tasks |
| 7 | SCIP, optional discovery и safe transformations | полезны после стабилизации exact agent read path |

## P0 — dogfood-ready correctness envelope

### Epic P0.1: Complete verified freshness and incremental lifecycle

Fast conservative source incrementality уже существует: identical
content/context/provenance переиспользуется без worker; source/API changes
инвалидируют affected snapshots; stale location не обслуживается как fresh.
P0 не строит это заново, а замыкает контракт на весь workspace.

Оставшийся scope:

- persistent workspace/component/dependency checkpoints;
- build configuration и resolved dependency graph как freshness inputs;
- explicit hit/miss planning для user-scoped dependency cache;
- один freshness gate для всех semantic query paths;
- failure/superseded-generation safety;
- cold, warm, changed-dependency и second-workspace fixtures;
- status, объясняющий reuse, reanalysis, partial и stale states.

Live watch, resumable large-workspace scheduler и parallel worker pools не
блокируют первый dogfood milestone. Они добавляются после появления реального
операционного bottleneck.

Текущие engineering baselines сохраняются для регрессий, но не являются
competitive benchmark: 1,200 sources / 13 batches дали около `83.2s` worker
time и `4.4s` SQLite commits; cold materialization 322 dependency blobs заняла
около `154.5s`; 389 MB входных JAR дали около 1.58 GB blobs. Эти наблюдения
обосновывают текущие cache/layout задачи, а сравнение с другими продуктами
проводится позже на dogfood-derived corpus.

Definition of done: explicit `kide index` переиспользует все compatible inputs,
инвалидирует только affected semantic owners и не позволяет ни одному query
вернуть stale/incompatible fact как `ok` при `fresh_only`.

Соответствующие Beads: `kide-yqp`, `kide-2a8`, `kide-zyp`; bounded scheduling
остается в последующем `kide-m6a`.

### Epic P0.2: Portable cross-artifact symbol identity

Цель: обеспечить точную композицию independently built dependency blobs.

Работы:

- ввести structured external key: ecosystem, coordinate/version при наличии,
  binary owner, member name и descriptor;
- преобразовывать K2 callable identity в точный JVM owner/name/descriptor,
  включая constructors, static members и overloads;
- отделить internal `SymbolId` от portable/interchange alias;
- перестать назначать external superclass/annotation hash текущего JAR;
- хранить unresolved external target как явный locator, не fabricated ID;
- resolver связывает locator с materialized artifact declaration;
- ambiguity остается явной;
- добавить fixtures с overloaded calls, inheritance и annotations через
  несколько JARs.

Definition of done: hierarchy/reference одного artifact разрешается в точную
declaration другого artifact независимо от порядка materialization; внешний
overloaded call связывается с declaration по descriptor, а не по одному имени.

Соответствующий Beads epic: `kide-8cn`.

## P1 — agent dogfooding и product read path

### Epic P1.1: Agent dogfooding before comparison

Первая agent integration использует стабильный CLI/JSON contract. Агент обязан:

- проверить status и при необходимости явно обновить index;
- работать по reusable IDs и candidates;
- использовать bounded fused context;
- корректно обрабатывать `stale`, `partial`, `ambiguous` и `unsupported`;
- записывать tool sequence, fallback source reads, latency и missing/wrong facts.

Read-only MCP добавляется после нескольких CLI dogfood sessions и остается
тонким adapter над тем же Core contract.

Definition of done: coding agent решает representative source и
dependency-aware задачи через KIDE; каждый обнаруженный semantic/product gap
классифицирован и превращен в focused Bead.

Соответствующий Beads epic: `kide-vpl`.

### Epic P1.2: Global dependency object catalog

Цель: дать headless-аналог JetBrains global class/symbol search.

Первая поверхность:

- classes/interfaces/enums/objects;
- methods/constructors/fields/properties;
- exact qualified-name и prefix search;
- signature/overload discrimination;
- component/artifact/version/scope filters;
- direct hierarchy;
- resolved applied annotations.

Требование к исполнению: query сначала использует catalog/postings и читает
только выбранные detail blocks; полный scan всех blobs не является нормальным
query path.

Definition of done: один запрос возвращает deterministic results из source
index, JDK и всех resolved dependency blobs с provenance и completeness.

Соответствующий Beads epic: `kide-vx4`; он развивается параллельно dogfooding и
не блокирует первые source-only agent sessions.

### Epic P1.3: Rich symbol and occurrence metadata

- documentation/docstrings;
- hyperlinkable signatures;
- read/write/import/generated/test roles;
- enclosing expression/declaration ranges;
- diagnostic source/tags;
- explicit override/type-definition/definition-alias relationships.

## P2 — безопасные действия и domain queries

### Epic P2.1: Competitive benchmark derived from dogfooding

Benchmark не блокирует рабочий продукт. Сначала KIDE используется собственным
coding agent; затем real task traces превращаются в frozen corpus.

Участники:

- IntelliJ/K2/javac как ground truth;
- KIDE;
- Codanna как strongest hybrid agent-product baseline;
- `scip-java` как compiler-index baseline;
- выбранные structural tools там, где они реально поддерживают задачу.

Метрики: precision/recall, unresolved/partial rate, cold/warm/incremental cost,
storage, agent tool calls, source fallbacks и tokens. Performance никогда не
публикуется отдельно от correctness.

Соответствующий Beads epic: `kide-0ba`, blocked by dogfood epic `kide-vpl`.

### Epic P2.2: SCIP interoperability and `scip-java` decision

Порядок:

1. SCIP export из собранного KIDE view;
2. conformance/snapshot tests и consumer smoke test;
3. SCIP import как explicit partial external facts;
4. решение по `scip-java`: oracle, import source или disposable backend.

SCIP остается потенциально lossy adapter, а не canonical schema.

Соответствующий Beads epic: `kide-v0z`.

### Epic P2.3: Optional semantic discovery layer

Сначала lexical/BM25 discovery по names, signatures, docs и paths. Локальный
vector index оценивается только на dogfood tasks. Ranked candidate несет
retrieval score, а дальнейшая навигация выполняется по exact KIDE identity;
similarity никогда не становится relationship precision.

Соответствующий Beads epic: `kide-nhl`.

### Epic P2.4: Semantic rename proof

```text
SymbolId
  → exact references
  → proposed WorkspaceEdit
  → diff preview
  → compiler validation
  → optional apply
```

Первый scope: Kotlin/JVM source symbols внутри одного workspace. Cross-repo и
binary modifications не входят в первую версию.

### Epic P2.5: Declarative applications в artifact schema

Сохранить `ApplicationFact` и typed arguments в dependency artifacts, добавить
postings по applied symbol и queries для framework annotations.

### Epic P2.6: Query packages и semantic projections

Развивать framework-specific packages поверх точного графа: Spring,
Jakarta Persistence и другие. Approximate architecture/impact projections
разрешены только с явной precision.

## P3 — distribution и расширение

### Epic P3.1: Shared semantic artifact registry

Локальный content-addressed cache остается source of behavior. Remote registry
добавляется как distribution/cache layer с проверкой digest, schema,
provenance и context identity.

### Epic P3.2: Дополнительные языки

Новый backend добавляется после прохождения общего correctness contract.
Количество parser languages не является самостоятельной целью.

### Epic P3.3: Optional LSP frontend

LSP имеет смысл как compatibility adapter для editors после стабилизации Core.
Он не должен становиться владельцем semantic state или обязательным runtime MCP.

## Идеи, не являющиеся текущими обязательствами

- Vector/lexical discovery кандидатов с последующим exact resolution; Codanna
  является product benchmark, но embedding index остается отдельным от exact
  semantic store.
- Shared public index популярных Maven artifacts.
- Offline JDK semantic artifact packs.
- Cross-repository impact analysis через portable symbol aliases.
- Dependency API diff между двумя artifact versions.
- Agent-oriented query plans, показывающие стоимость и completeness до запуска.
- Semantic evidence bundles: минимальный набор declarations/edges, достаточный
  для проверки ответа агентом.

## Критерий успешного позиционирования

KIDE занимает заявленную нишу, когда можно воспроизводимо показать следующий
сценарий:

1. Новый workspace разрешает Kotlin/JVM project model.
2. Большая часть dependency semantics берется из уже существующих immutable
   blobs.
3. Агент сразу выполняет глобальный поиск по source, JDK и dependencies.
4. Definition/references/calls различают overloads и language interop.
5. Каждый ответ сообщает provenance, freshness, completeness и precision.
6. Для повторных запросов не требуется resident LSP/K2/IDE process.
7. Correctness не уступает выбранному compiler/IDE baseline, а latency и reuse
   измерены публичным benchmark.

До выполнения этих условий архитектурная ниша считается хорошо обоснованной,
но продуктовое преимущество — еще не доказанным.

## Итоговая competitive position

KIDE не является первым persistent code index и не единственным способом
обслуживать semantic facts без LSP. Его потенциально защищаемая позиция уже:

> **Local agent-native JVM semantic index, который materializes
> compiler-resolved identities и relationships один раз, глобально запрашивает
> source, JDK и binary dependencies, и повторно использует content-addressed
> dependency knowledge между workspace без resident IDE/LSP.**

`ast-index`, Codanna и `codebase-memory-mcp` подтверждают спрос на быстрый
agent-facing local index. SCIP и Kythe подтверждают precomputed semantic
architecture. IntelliJ подтверждает ценность глобального project+library
object universe. Ни один из них сам по себе не доказывает преимущество KIDE;
вместе они показывают, где именно это преимущество необходимо измерить.

Текущее состояние следует формулировать без смешения vision и результата:

- **Codanna сегодня сильнее как готовый agent code-intelligence product:** MCP,
  vector discovery, fused context, watch mode, много языков и опубликованные
  performance baselines.
- **KIDE уже имеет более сильный источник JVM source facts на покрытых
  сценариях:** K2/javac-resolved identity, explicit quality/provenance и binary
  declarations по descriptors.
- **Главный потенциальный moat KIDE — dependencies:** глобальные JDK/JAR
  objects и reuse independently built semantic artifacts между workspace.
- **Этот moat еще не доказан полностью:** external overload mapping,
  cross-artifact identity, cold/storage efficiency и agent MCP остаются
  обязательными milestones.

Итоговый продуктовый тезис:

> **Codanna помогает агенту быстро найти правдоподобно нужный код. KIDE должен
> позволить агенту безопасно действовать на точных JVM-фактах во всем build
> universe, не удерживая runtime LSP/IDE и не переиндексируя одинаковые
> dependencies для каждого workspace.**
