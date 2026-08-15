package dev.kide.worker

import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive
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
        externalTargets: Map<String, String> = emptyMap(),
        hierarchy: List<K2HierarchyEdge> = emptyList(),
        annotations: List<K2ResolvedAnnotation> = emptyList(),
    ): List<JsonElement> {
        val sourceTargets = targetIds(snapshots)
        val targets = sourceTargets + externalTargets.filterKeys { it !in sourceTargets }
        val bySource = resolved.groupBy { Path.of(it.sourcePath).toAbsolutePath().normalize() }
        val annotationsBySource = annotations.groupBy { Path.of(it.sourcePath).toAbsolutePath().normalize() }
        return snapshots.map { snapshot -> enrichSnapshot(snapshot.jsonObject, workspaceRoot, bySource, annotationsBySource, targets, hierarchy) }
    }

    private fun enrichSnapshot(
        snapshot: JsonObject,
        workspaceRoot: Path,
        bySource: Map<Path, List<K2ResolvedReference>>,
        annotationsBySource: Map<Path, List<K2ResolvedAnnotation>>,
        targets: Map<String, String>,
        hierarchy: List<K2HierarchyEdge>,
    ): JsonElement {
        val sourceUnit = snapshot["source_unit"]!!.jsonObject
        val path = workspaceRoot.resolve(sourceUnit.requiredString("path")).toAbsolutePath().normalize()
        val contents = Files.readString(path)
        val provenance = snapshot["provenance"]!!
        val owners = enclosingSymbols(snapshot["symbols"]!!.jsonArray)
        val snapshotSymbols = snapshot["symbols"]!!.jsonArray.map { it.jsonObject.requiredString("id") }.toSet()
        val annotationTargets = annotationsBySource[path].orEmpty().mapNotNull { annotation ->
            val owner = targets[annotation.ownerKey] ?: return@mapNotNull null
            val target = targets[annotation.targetKey] ?: return@mapNotNull null
            owner to target
        }.groupBy({ it.first }, { it.second }).mapValues { (_, targets) -> targets.distinct().sorted() }
        val hierarchyFacts = hierarchy.mapNotNull { edge ->
            val subtype = targets[edge.subtypeKey] ?: return@mapNotNull null
            val supertype = targets[edge.supertypeKey] ?: return@mapNotNull null
            if (subtype !in snapshotSymbols) return@mapNotNull null
            buildJsonObject {
                put("subtype", subtype)
                put("supertype", supertype)
                put("precision", "exact")
                put("provenance", provenance)
            }
        }.distinctBy { it.toString() }
        val exact = bySource[path].orEmpty().mapNotNull { reference ->
            val target = targets[reference.targetKey] ?: return@mapNotNull null
            val start = utf8Offset(contents, reference.startUtf16)
            val end = utf8Offset(contents, reference.endUtf16)
            ExactFact(
                reference,
                target,
                occurrence(sourceUnit.requiredString("id"), start, end, reference, target, enclosingSymbol(owners, start, end), provenance),
            )
        }.distinctBy { it.reference.sourcePath to it.reference.startUtf16 to it.reference.endUtf16 to it.target }
        if (exact.isEmpty() && hierarchyFacts.isEmpty() && annotationTargets.isEmpty()) return snapshot

        val exactRanges = exact.map { rangeKey(it.occurrence) }.toSet()
        val remainingOccurrences = snapshot["occurrences"]!!.jsonArray.filter { rangeKey(it) !in exactRanges }
        return buildJsonObject {
            snapshot.forEach { (key, value) ->
                if (key !in setOf("symbols", "occurrences", "references", "calls", "types", "hierarchy")) put(key, value)
            }
            put("symbols", buildJsonArray {
                snapshot["symbols"]!!.jsonArray.forEach { element ->
                    val symbol = element.jsonObject
                    val targets = annotationTargets[symbol.requiredString("id")].orEmpty()
                    add(buildJsonObject {
                        symbol.forEach { (key, value) -> if (key != "applied_symbols") put(key, value) }
                        put("applied_symbols", buildJsonArray { targets.forEach { add(JsonPrimitive(it)) } })
                    })
                }
            })
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
                        put("caller", fact.occurrence.jsonObject["enclosing_symbol"] ?: JsonNull)
                        put("precision", "exact")
                    })
                }
            })
            put("types", buildJsonArray {
                exact.mapNotNull { it.reference.typeDisplay }.distinct().sorted().forEach { display ->
                    add(typeRecord(display, provenance))
                }
            })
            put("hierarchy", buildJsonArray { hierarchyFacts.forEach(::add) })
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
                "constructor" -> constructorKey(qualifiedName, symbol["signature"]?.jsonPrimitive?.content)?.let { it to id }
                "function", "property" -> callableKey(qualifiedName, symbol["signature"]?.jsonPrimitive?.content, member = false) to id
                "method", "field" -> callableKey(qualifiedName, symbol["signature"]?.jsonPrimitive?.content, member = true) to id
                else -> null
            }
        }
        .groupBy({ it.first }, { it.second })
        .mapNotNull { (key, ids) -> ids.distinct().singleOrNull()?.let { key to it } }
        .toMap()

    private fun callableKey(qualifiedName: String, signature: String?, member: Boolean): String {
        val owner = qualifiedName.substringBeforeLast('.', missingDelimiterValue = "")
        val name = qualifiedName.substringAfterLast('.')
        val separator = if (member) "." else "/"
        return "callable:${owner.replace('.', '/')}$separator$name#${parameterSignature(signature)}"
    }

    private fun constructorKey(qualifiedName: String, signature: String?): String? {
        val owner = qualifiedName.removeSuffix(".<init>")
        if (owner == qualifiedName) return null
        val packageName = owner.substringBeforeLast('.', missingDelimiterValue = "")
        val className = owner.substringAfterLast('.')
        return "callable:${packageName.replace('.', '/')}/${className}.${className}#${parameterSignature(signature)}"
    }

    private fun parameterSignature(signature: String?): String = signature
        ?.substringBeforeLast(":", signature)
        ?.replace(" ", "")
        ?.replace("Int", "kotlin/Int")
        ?.replace("String", "kotlin/String")
        ?.replace("Boolean", "kotlin/Boolean")
        ?.replace("Long", "kotlin/Long")
        ?.replace("Double", "kotlin/Double")
        ?.replace("Float", "kotlin/Float")
        ?: "(?)"

    private fun occurrence(
        sourceUnit: String,
        start: Int,
        end: Int,
        reference: K2ResolvedReference,
        target: String,
        enclosingSymbol: String?,
        provenance: JsonElement,
    ): JsonElement = buildJsonObject {
        put("range", buildJsonObject {
            put("source_unit", sourceUnit)
            put("bytes", buildJsonObject {
                put("start", start)
                put("end", end)
            })
        })
        put("kind", if (reference.isCall) "call" else "reference")
        put("enclosing_symbol", enclosingSymbol)
        put("target", target)
        put("type_id", reference.typeDisplay?.let(::typeId))
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

    private fun typeRecord(display: String, provenance: JsonElement): JsonElement = buildJsonObject {
        put("id", typeId(display))
        put("language", "kotlin")
        put("display", display)
        put("backend_key", buildJsonObject {
            put("backend", WORKER_NAME)
            put("schema_version", 1)
            put("value", display)
        })
        put("freshness", "fresh")
        put("completeness", "complete")
        put("provenance", provenance)
    }

    private fun typeId(display: String): String {
        val digest = MessageDigest.getInstance("SHA-256").digest(display.encodeToByteArray())
        return "kotlin:type:${digest.joinToString("") { "%02x".format(it) }}"
    }

    private fun enclosingSymbols(symbols: List<JsonElement>): List<EnclosingSymbol> = symbols.mapNotNull { element ->
        val symbol = element.jsonObject
        val declaration = symbol["declaration"]!!.jsonObject["bytes"]!!.jsonObject
        val start = declaration.requiredString("start").toInt()
        val end = declaration.requiredString("end").toInt()
        symbol.requiredString("id").let { id -> EnclosingSymbol(id, start, end) }
    }

    private fun enclosingSymbol(owners: List<EnclosingSymbol>, start: Int, end: Int): String? = owners
        .filter { owner -> owner.start <= start && end <= owner.end && (owner.start != start || owner.end != end) }
        .minByOrNull { owner -> owner.end - owner.start }
        ?.id

    private data class ExactFact(val reference: K2ResolvedReference, val target: String, val occurrence: JsonElement)
    private data class EnclosingSymbol(val id: String, val start: Int, val end: Int)
}
