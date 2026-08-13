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
        val target = use["calls"]!!.jsonArray.single().jsonObject["target"]!!.toString()
        assertTrue(target.contains("One.kt") && target.contains("constructor"))
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
