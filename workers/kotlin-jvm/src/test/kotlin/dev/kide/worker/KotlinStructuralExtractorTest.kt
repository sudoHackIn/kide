package dev.kide.worker

import java.nio.file.Files
import java.nio.file.Path
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

class KotlinStructuralExtractorTest {
    @Test
    fun emitsDeterministicStructuralFactsWithoutSemanticTargets() {
        val root = Files.createTempDirectory("kide-psi-structural-")
        val source = """
            package fixture
            import kotlin.collections.List
            typealias Nickname = String
            @Deprecated("fixture")
            class Outer(val property: Nickname) {
                constructor() : this("default")
                class Nested
                fun member(value: Nickname): String {
                    val local = value
                    return local.trim()
                }
            }
            fun topLevel(items: List<String>) = Outer("π").member(items.first())
            fun topLevel(items: String) = items
            fun String.extension() = length
        """.trimIndent()
        Files.writeString(root.resolve("Example.kt"), source)

        val first = KotlinStructuralExtractor().use { it.analyze(sourceUnit("Example.kt"), root).jsonObject }
        val second = KotlinStructuralExtractor().use { it.analyze(sourceUnit("Example.kt"), root).jsonObject }

        assertEquals(first, second)
        assertTrue(first["structural_fingerprint"]!!.jsonPrimitive.content.startsWith("sha256:"))
        assertTrue(first["public_api_fingerprint"]!!.jsonPrimitive.content.startsWith("sha256:"))
        val symbols = first["symbols"]!!.jsonArray.map { it.jsonObject }
        assertTrue(symbols.any { it["name"]!!.jsonPrimitive.content == "Outer" && it["kind"]!!.jsonPrimitive.content == "class" })
        assertTrue(symbols.any { it["name"]!!.jsonPrimitive.content == "member" && it["kind"]!!.jsonPrimitive.content == "method" })
        assertTrue(symbols.any { it["name"]!!.jsonPrimitive.content == "topLevel" && it["kind"]!!.jsonPrimitive.content == "function" })
        assertTrue(symbols.any { it["name"]!!.jsonPrimitive.content == "Nickname" && it["kind"]!!.jsonPrimitive.content == "type_alias" })
        assertTrue(symbols.any { it["name"]!!.jsonPrimitive.content == "local" && it["kind"]!!.jsonPrimitive.content == "property" })
        assertTrue(symbols.any { it["name"]!!.jsonPrimitive.content == "Nested" && it["kind"]!!.jsonPrimitive.content == "class" })
        assertTrue(symbols.count { it["name"]!!.jsonPrimitive.content == "topLevel" } == 2)
        assertTrue(symbols.any { it["name"]!!.jsonPrimitive.content == "<init>" && it["kind"]!!.jsonPrimitive.content == "constructor" })
        assertTrue(symbols.single { it["name"]!!.jsonPrimitive.content == "Outer" }["annotations"]!!.jsonArray.any { it.jsonPrimitive.content == "Deprecated" })
        assertTrue(first["references"]!!.jsonArray.isEmpty())
        assertTrue(first["calls"]!!.jsonArray.isEmpty())
        assertTrue(first["occurrences"]!!.jsonArray.all { occurrence -> occurrence.jsonObject["target"] == null || occurrence.jsonObject["target"]!!.toString() == "null" })
        assertTrue(first["occurrences"]!!.jsonArray.any { it.jsonObject["kind"]!!.jsonPrimitive.content == "call" })
        assertTrue(first["occurrences"]!!.jsonArray.any { it.jsonObject["kind"]!!.jsonPrimitive.content == "import" })
    }

    @Test
    fun reportsParseErrorsAndConvertsUnicodePsiOffsetsToUtf8Bytes() {
        val root = Files.createTempDirectory("kide-psi-unicode-")
        val source = "fun emoji() = \"😀\"\nclass Broken {"
        Files.writeString(root.resolve("Unicode.kt"), source)

        val snapshot = KotlinStructuralExtractor().use { it.analyze(sourceUnit("Unicode.kt"), root).jsonObject }

        assertTrue(snapshot["diagnostics"]!!.jsonArray.isNotEmpty())
        val emojiSymbol = snapshot["symbols"]!!.jsonArray
            .map { it.jsonObject }
            .single { it["name"]!!.jsonPrimitive.content == "emoji" }
        val start = emojiSymbol["name_range"]!!.jsonObject["bytes"]!!.jsonObject["start"]!!.jsonPrimitive.content.toInt()
        assertEquals(source.indexOf("emoji".encodeToByteArray().decodeToString()), start)
    }

    private fun sourceUnit(path: String) = buildJsonObject {
        put("id", "gradle::fixture:main:$path")
        put("component", "gradle::fixture:main")
        put("path", path)
        put("language", "kotlin")
        put("origin", "source")
        put("content", "sha256:test")
        put("context", "sha256:context")
    }
}
