# `codebase-memory-mcp`: разбор применительно к KIDE

- Статус: зафиксированный конкурентный разбор
- Проверено: 2026-08-21
- Проект: [`DeusData/codebase-memory-mcp`](https://github.com/DeusData/codebase-memory-mcp)
- Проверенная ревизия: `010569f`

## Вывод

`codebase-memory-mcp` — сильный agent-oriented продукт для быстрого построения
карты репозитория, поиска контекста и графовых запросов. Он конкурирует с KIDE
за внимание пользователя, качество MCP UX и экономию токенов агента, но не
является эквивалентом compiler-authoritative semantic index.

Для JVM его граф нельзя считать источником точных answers для overload
resolution, calls, references и cross-language navigation. В основе лежат
структурный разбор и последующее связывание сущностей; когда компиляторной
идентичности недостаточно, система вынуждена сопоставлять декларации по именам
и другим эвристическим признакам.

Краткая классификация:

| Измерение | Оценка |
| --- | --- |
| Agent/MCP UX | сильная сторона |
| Быстрое знакомство с репозиторием | сильная сторона |
| Ширина языков | сильная сторона |
| Архитектурный и impact graph | полезен как приблизительное представление |
| Natural-language retrieval | полезный дополнительный канал |
| Точные JVM references/calls | недостаточно надежны |
| Overloads и compiler-selected targets | модель идентичности недостаточна |
| Безопасные semantic transformations | не являются ядром продукта |

## Что именно было проверено

Индексатор был собран и запущен на репозитории KIDE с умеренным профилем.
Полученный граф содержал 2 265 nodes и 13 238 edges; построение заняло примерно
5,5 секунды на проверенной машине. Это хороший результат для первоначального
agent context и демонстрирует зрелую упаковку продукта.

Однако в графе были обнаружены семантически ложные call edges:

- Kotlin-вызов `Path.normalize()` был связан с одноименным Rust-методом
  `FrameworkQueryResult.normalize`;
- Kotlin-вызов `check` был связан с несвязанной project declaration;
- присутствовала ложная self-call связь.

Отдельная минимальная fixture с перегруженными Java- и Kotlin-методами показала
более фундаментальную проблему: overloads схлопнулись в один symbol и один
`CALLS` edge. Причина видна в модели хранения: qualified identity не включает
полную сигнатуру метода, а уникальность декларации обеспечивается сочетанием
project и qualified name.

Это не означает, что весь граф неверен. Это означает, что его ребра нельзя без
дополнительной валидации использовать как доказанные compiler facts.

## Что означает слово «semantic» в этом продукте

В `codebase-memory-mcp` пересекаются три разных значения:

1. Структурная семантика: классы, методы, containment и импортоподобные связи,
   извлеченные из syntax tree.
2. Graph semantics: traversal, impact paths, neighborhoods и архитектурные
   агрегаты над сохраненными nodes/edges.
3. Retrieval semantics: embeddings или natural-language similarity для поиска
   релевантного кода.

Ни одно из них автоматически не означает compiler semantic resolution.
Compiler semantics отвечает на более строгий вопрос: какую конкретно
декларацию, с учетом overloads, generic substitution, receivers, imports,
classpath и language interop, выбрал компилятор в этой точке программы.

Поэтому embedding search может быть отличным retrieval-инструментом и
одновременно не давать гарантий для `definition`, `references` или `callers`.

## Сопоставление с KIDE

| Возможность | `codebase-memory-mcp` | KIDE |
| --- | --- | --- |
| Первичный индекс | syntax/structural graph | compiler-derived normalized facts |
| Symbol identity | преимущественно source-qualified identity | opaque stable ID + backend key + JVM descriptors |
| Calls | graph edge, возможны эвристические совпадения | first-class edge с precision и selected target |
| Quality metadata | не является центральным контрактом | freshness, completeness, precision, provenance |
| Dependency reuse | не основной storage primitive | content-addressed immutable artifact blobs |
| Agent surface | готовый MCP-first продукт | CLI есть, MCP предстоит оформить |
| Языки | очень широкое покрытие | намеренно Kotlin/JVM-first |
| Approximate semantic search | есть | возможен позднее как отдельный слой |

KIDE не должен пытаться выиграть количеством parser languages или просто
наличием code graph. Его сильная территория начинается там, где неправильный
edge хуже отсутствующего: JVM overload resolution, implementations, exact
callers, Java/Kotlin interop и dependency objects.

## Что стоит перенять

### 1. Agent-first упаковку

Пользователь должен получать полезный результат после одной команды, без
понимания внутренней архитектуры индексатора. MCP должен предоставлять
небольшой набор хорошо различимых tools, а не отражать внутренние таблицы KIDE.

### 2. Progressive disclosure

Агенту сначала нужны candidates, summaries и bounded neighborhoods. Полную
декларацию или большой graph fragment следует читать только после выбора.
Секционные dependency blobs KIDE уже хорошо подходят для такого поведения.

### 3. Архитектурные и impact-запросы

Даже приблизительные graph queries полезны, если ответ явно помечен как
`approximate`. KIDE может строить такие projections поверх точного ядра, не
смешивая их с exact references.

### 4. Hybrid retrieval как дополнительный канал

Lexical/vector retrieval может находить неизвестный агенту `SymbolId`. После
этого navigation должна переходить на compiler-derived graph. Retrieval
никогда не должен молча превращаться в exact semantic answer.

### 5. Метрику экономии agent context

Кроме latency нужно измерять количество прочитанных файлов, tool calls и
source tokens, необходимых агенту для решения задачи.

## Что не стоит перенимать

- Идентичность callable без полной сигнатуры.
- Silent guessing при неоднозначном symbol resolution.
- Один и тот же тип edge для доказанных и предположительных связей.
- Приоритет ширины языков над correctness первого backend.
- Маркетинговое объединение embeddings, syntax graph и compiler semantics под
  одним недифференцированным словом `semantic`.

## Роль в стратегии KIDE

`codebase-memory-mcp` следует использовать как:

- продуктовый benchmark MCP onboarding и tool ergonomics;
- baseline agent-efficiency;
- approximate/structural baseline в correctness benchmark;
- источник идей для bounded graph exploration.

Его не следует использовать как основной correctness baseline JVM semantics.
Для этой роли важнее `scip-java`, IntelliJ и compiler-native extraction.
