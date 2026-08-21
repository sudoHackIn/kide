# `Claude-ast-index-search`: разбор применительно к KIDE

- Статус: зафиксированный конкурентный разбор
- Проверено: 2026-08-21
- Проект: [`defendend/Claude-ast-index-search`](https://github.com/defendend/Claude-ast-index-search)
- Проверенная версия: `3.50.0`, revision `e499dcc`

## Вывод

`ast-index` — прямой конкурент пользовательской поверхности KIDE: локальный
persistent index, быстрый CLI, incremental update, agent plugins и MCP без
resident LSP. Команды почти буквально совпадают с ожидаемыми KIDE queries:
`search`, `class`, `usages`, `implementations`, `hierarchy` и `callers`.

Но это не compiler-authoritative semantic engine. В проверенной реализации
declarations извлекаются Tree-sitter parsers, references — преимущественно
лексическими эвристиками, а связи хранятся и запрашиваются по строковым именам.
Поэтому продукт является сильным benchmark agent UX и latency, но слабым
correctness baseline для JVM.

Ключевое различие:

```text
ast-index: syntax → names → heuristic/string relationships → SQLite
KIDE:      compiler/build authority → resolved identities → typed facts → store
```

Tree-sitter сам по себе не является достаточной причиной считать продукт
проигравшим. Он дает `ast-index` широкое покрытие языков, дешевую индексацию и
простую эксплуатацию. Ограничение появляется там, где Tree-sitter extraction
не дополнен полноценным type/classpath resolution.

## Что представляет собой продукт

`ast-index` строит SQLite database в системном cache directory. В ней хранятся:

- files и source roots;
- declarations и текстовые signatures;
- references;
- inheritance;
- project modules и module dependencies;
- Android resources/XML usages;
- iOS assets/storyboard usages;
- FTS5 index по names и signatures.

Индекс можно полностью перестроить или incrementally обновить. MCP server
является тонкой stdio-оболочкой, которая запускает CLI для каждого tool call.
Это удачное разделение: индекс не требует daemon, а MCP можно обновлять
независимо от основного binary.

См. [database schema](https://github.com/defendend/Claude-ast-index-search/blob/e499dcc6fcc90dfceafb629fbf5289824a40cccb/docs/db-schema.md)
и [MCP setup](https://github.com/defendend/Claude-ast-index-search/blob/e499dcc6fcc90dfceafb629fbf5289824a40cccb/docs/mcp-setup.md).

## Фактическая семантика основных запросов

### `usages`

Таблица `refs` содержит `file_id`, `name`, `line` и context. У reference нет
target symbol ID. Документация схемы прямо фиксирует, что `refs.name` не
ссылается на конкретную строку `symbols`.

Для Kotlin и Java generic reference extractor ищет:

- CamelCase identifiers как предполагаемые types;
- lowercase identifiers перед `(` как предполагаемые calls;
- исключает списки keywords и локально объявленные names.

Затем `usages X` выполняет `WHERE refs.name = X`. При отсутствии записей
команда откатывается к grep-like поиску. Это быстрый поиск упоминаний, но не
ответ на вопрос «какая declaration выбрана компилятором?».

См. [reference extraction](https://github.com/defendend/Claude-ast-index-search/blob/e499dcc6fcc90dfceafb629fbf5289824a40cccb/src/parsers/mod.rs#L716-L979)
и [query implementation](https://github.com/defendend/Claude-ast-index-search/blob/e499dcc6fcc90dfceafb629fbf5289824a40cccb/src/commands/index.rs#L701-L790).

### `implementations`

`inheritance` связывает resolved child row со строкой `parent_name`. Поиск
implementation сравнивает exact parent name и qualified suffix через `LIKE`.
Parent declaration может отсутствовать в index и не имеет foreign key на
`symbols`.

Для Kotlin тип родителя извлекается из syntax node. Различие `extends` и
`implements` восстанавливается по форме delegation specifier, а не по
compiler-resolved kind родителя.

Это полезно для уникальных names внутри обычного repository, но дает ambiguity
при одинаковых simple names, aliases, imports и external dependencies.

См. [Kotlin inheritance extraction](https://github.com/defendend/Claude-ast-index-search/blob/e499dcc6fcc90dfceafb629fbf5289824a40cccb/src/parsers/treesitter/kotlin.rs#L197-L325).

### `callers` и call tree

`callers` не читает persistent semantic call graph. Команда строит regex для
`function_name(`, ищет совпадения по source files, пропускает похожие
declarations и сканирует строки назад, чтобы найти enclosing function.

Следствия:

- overload target не различается;
- receiver type не разрешается;
- одноименные methods разных classes смешиваются;
- indirect/interface dispatch не моделируется;
- результат зависит от распознавания enclosing declaration regex-ом.

См. [caller implementation](https://github.com/defendend/Claude-ast-index-search/blob/e499dcc6fcc90dfceafb629fbf5289824a40cccb/src/commands/grep.rs#L166-L360).

### Qualified identity и dependencies

`symbols` допускает `qualified_name` и `signature`, но common parser result не
несет portable resolved identity. В проверенной indexing path отдельное
вычисление qualified names применяется для C++; Kotlin/Java остаются в
основном name/signature-oriented.

Dependency commands моделируют главным образом project/module graph. Есть
специальная индексация TypeScript `.d.ts`, но нет аналогичного compiler
classpath/JAR object catalog для JVM. Поэтому команда не может глобально
искать точные classes/methods/fields во всех binary dependencies так, как это
делает IntelliJ или как планирует KIDE.

## Сопоставление с KIDE

| Возможность | `ast-index` | KIDE |
| --- | --- | --- |
| Primary authority | Tree-sitter + regex/heuristics | Kotlin/Java compiler и bytecode authorities |
| Persistent storage | SQLite per project | normalized source store + immutable dependency blobs |
| Symbol lookup | names, optional textual qname/signature | resolved identity и JVM descriptor |
| References | name occurrences | target-aware facts с precision/provenance |
| Calls | grep/heuristic callers | selected targets как first-class semantic edges |
| Implementations | parent-name matching | resolved hierarchy across source и artifacts |
| JVM dependencies | module metadata, без JAR symbol universe | content-addressed dependency object catalog |
| MCP/agent UX | уже готов и хорошо упакован | предстоит оформить |
| Languages | очень широкое покрытие | Kotlin/JVM-first |

## Где `ast-index` сильнее KIDE сегодня

- Законченное onboarding и distribution.
- Широкая языковая и platform-specific поверхность.
- MCP, plugins и agent rules уже существуют.
- Богатый набор discoverable CLI commands.
- Incremental update и cache lifecycle проработаны как пользовательская функция.
- Публикуются latency и token-efficiency claims.

Поэтому нельзя строить позиционирование только на фразе «мы используем не
Tree-sitter». Агент покупает outcome. KIDE должен показать, на каких реальных
задачах compiler accuracy меняет результат.

## Что стоит перенять

1. Имена и granularity agent tools: `outline`, `usages`, `implementations`,
   `changed`, bounded scopes.
2. Thin MCP adapter над стабильным CLI/query contract.
3. One-command install, project discovery и walk-up к index root.
4. Incremental `update` как явную product capability.
5. Compact text output по умолчанию и JSON по запросу.
6. Platform/domain queries как projections, а не разрастание core schema.

## Что не стоит перенимать

- String reference как эквивалент resolved edge.
- Команду `callers`, имя которой обещает больше, чем гарантирует алгоритм.
- Silent fallback от semantic query к grep без изменения quality contract.
- Смешивание exact и approximate answers в одном output type.
- Ширину языков как основной success metric.

## Роль в стратегии и benchmark

`ast-index` должен быть:

- прямым product/UX competitor;
- baseline latency и agent token efficiency;
- representative baseline класса Tree-sitter persistent indexes;
- обязательным участником JVM correctness corpus.

Его не нужно использовать как ground truth. Ground truth дают compiler/IDE
authorities; `ast-index` показывает, насколько далеко можно дойти более дешевым
структурным подходом.
