package dev.kide.worker

import java.nio.file.Files
import java.nio.file.Path
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

/** Joins K2-resolved targets to the PSI-derived, canonical KIDE symbol IDs. */
internal object K2SnapshotEnricher {
    fun enrich(
        snapshots: List<JsonElement>,
        workspaceRoot: Path,
        resolved: List<K2ResolvedReference>,
    ): List<JsonElement> {
        val targets = targetIds(snapshots)
        val bySource = resolved.groupBy { Path.of(it.sourcePath).toAbsolutePath().normalize() }
        return snapshots.map { snapshot -> enrichSnapshot(snapshot.jsonObject, workspaceRoot, bySource, targets) }
    }

    private fun enrichSnapshot(
        snapshot: JsonObject,
        workspaceRoot: Path,
        bySource: Map<Path, List<K2ResolvedReference>>,
        targets: Map<String, String>,
    ): JsonElement {
        val sourceUnit = snapshot["source_unit"]!!.jsonObject
        val path = workspaceRoot.resolve(sourceUnit.requiredString("path")).toAbsolutePath().normalize()
        val contents = Files.readString(path)
        val provenance = snapshot["provenance"]!!
        val exact = bySource[path].orEmpty().mapNotNull { reference ->
            val target = targets[reference.targetKey] ?: return@mapNotNull null
            ExactFact(reference, target, occurrence(sourceUnit.requiredString("id"), contents, reference, target, provenance))
        }.distinctBy { it.reference.sourcePath to it.reference.startUtf16 to it.reference.endUtf16 to it.target }
        if (exact.isEmpty()) return snapshot

        val exactRanges = exact.map { rangeKey(it.occurrence) }.toSet()
        val remainingOccurrences = snapshot["occurrences"]!!.jsonArray.filter { rangeKey(it) !in exactRanges }
        return buildJsonObject {
            snapshot.forEach { (key, value) ->
                if (key !in setOf("occurrences", "references", "calls")) put(key, value)
            }
            put("occurrences", buildJsonArray {
                (remainingOccurrences + exact.map { it.occurrence }).forEach(::add)
            })
            put("references", buildJsonArray {
                exact.filterNot { it.reference.isCall }.forEach { fact ->
                    add(buildJsonObject {
                        put("source", fact.occurrence)
                        put("target", fact.target)
                        put("precision", "exact")
                    })
                }
            })
            put("calls", buildJsonArray {
                exact.filter { it.reference.isCall }.forEach { fact ->
                    add(buildJsonObject {
                        put("source", fact.occurrence)
                        put("target", fact.target)
                        put("caller", null)
                        put("precision", "exact")
                    })
                }
            })
        }
    }

    private fun targetIds(snapshots: List<JsonElement>): Map<String, String> = snapshots
        .flatMap { snapshot -> snapshot.jsonObject["symbols"]!!.jsonArray }
        .mapNotNull { element ->
            val symbol = element.jsonObject
            val id = symbol.requiredString("id")
            val qualifiedName = symbol["qualified_name"]?.jsonPrimitive?.content ?: return@mapNotNull null
            when (symbol.requiredString("kind")) {
                "class", "interface", "enum", "object" -> "class:$qualifiedName" to id
                "constructor" -> constructorKey(qualifiedName)?.let { it to id }
                "function", "method", "property", "field" -> callableKey(qualifiedName) to id
                else -> null
            }
        }
        .groupBy({ it.first }, { it.second })
        .mapNotNull { (key, ids) -> ids.distinct().singleOrNull()?.let { key to it } }
        .toMap()

    private fun callableKey(qualifiedName: String): String {
        val owner = qualifiedName.substringBeforeLast('.', missingDelimiterValue = "")
        val name = qualifiedName.substringAfterLast('.')
        return "callable:${owner.replace('.', '/')}.${name}"
    }

    private fun constructorKey(qualifiedName: String): String? {
        val owner = qualifiedName.removeSuffix(".<init>")
        if (owner == qualifiedName) return null
        val packageName = owner.substringBeforeLast('.', missingDelimiterValue = "")
        val className = owner.substringAfterLast('.')
        return "callable:${packageName.replace('.', '/')}/${className}.${className}"
    }

    private fun occurrence(
        sourceUnit: String,
        contents: String,
        reference: K2ResolvedReference,
        target: String,
        provenance: JsonElement,
    ): JsonElement = buildJsonObject {
        put("range", buildJsonObject {
            put("source_unit", sourceUnit)
            put("bytes", buildJsonObject {
                put("start", utf8Offset(contents, reference.startUtf16))
                put("end", utf8Offset(contents, reference.endUtf16))
            })
        })
        put("kind", if (reference.isCall) "call" else "reference")
        put("enclosing_symbol", null)
        put("target", target)
        put("type_id", null)
        put("precision", "exact")
        put("freshness", "fresh")
        put("completeness", "complete")
        put("provenance", provenance)
    }

    private fun rangeKey(occurrence: JsonElement): Pair<String, String> {
        val range = occurrence.jsonObject["range"]!!.jsonObject
        return range.requiredString("source_unit") to range["bytes"]!!.toString()
    }

    private fun utf8Offset(contents: String, utf16Offset: Int): Int {
        require(utf16Offset in 0..contents.length) { "K2 offset outside source text" }
        return contents.substring(0, utf16Offset).encodeToByteArray().size
    }

    private data class ExactFact(val reference: K2ResolvedReference, val target: String, val occurrence: JsonElement)
}
