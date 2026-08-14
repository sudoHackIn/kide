package dev.kide.worker

import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put

class KotlinStructuralBatchTest {
    @Test
    fun analyzesMultipleKotlinSourceUnitsInOneWorkerBatch() {
        val root = Files.createTempDirectory("kide-psi-batch-")
        Files.writeString(root.resolve("One.kt"), "package fixture\nclass One")
        Files.writeString(root.resolve("Two.kt"), "package fixture\nfun create() = One()")
        val payload = buildJsonObject {
            put("source_units", buildJsonArray {
                add(sourceUnit("One.kt"))
                add(sourceUnit("Two.kt"))
            })
        }

        val result = structuralBatch(payload, root)

        assertEquals(2, result.jsonObject["snapshots"]!!.jsonArray.size)
        val use = result.jsonObject["snapshots"]!!.jsonArray
            .map { it.jsonObject }
            .single { it["source_unit"]!!.jsonObject["path"]!!.toString().contains("Two.kt") }
        assertEquals(1, use["calls"]!!.jsonArray.size)
        val call = use["calls"]!!.jsonArray.single().jsonObject
        val target = call["target"]!!.toString()
        assertTrue(target.contains("One.kt") && target.contains("constructor"))
        assertTrue(call["caller"]!!.toString().contains("Two.kt") && call["caller"]!!.toString().contains("function:create"))
    }

    @Test
    fun mapsTheExactSelectedSourceOverload() {
        val root = Files.createTempDirectory("kide-k2-overload-")
        Files.writeString(root.resolve("Overload.kt"), """
            package fixture
            fun pick(value: Int) = value
            fun pick(value: String) = value
            fun use() = pick(1)
        """.trimIndent())
        val payload = buildJsonObject {
            put("source_units", buildJsonArray { add(sourceUnit("Overload.kt")) })
        }

        val snapshot = structuralBatch(payload, root).jsonObject["snapshots"]!!.jsonArray.single().jsonObject

        val call = snapshot["calls"]!!.jsonArray.single().jsonObject
        val expected = snapshot["symbols"]!!.jsonArray.map { it.jsonObject }.single { symbol ->
            symbol["name"]!!.toString().contains("pick") && symbol["signature"]!!.toString().contains("Int")
        }["id"]!!.toString()
        assertEquals(expected, call["target"]!!.toString())
        val typeId = call["source"]!!.jsonObject["type_id"]
        assertTrue(typeId != null && typeId.toString().contains("kotlin:type:"))
        assertTrue(snapshot["types"]!!.jsonArray.any { type -> type.jsonObject["id"] == typeId })
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
