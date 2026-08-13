package dev.kide.worker

import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put

class KotlinStructuralBatchTest {
    @Test
    fun analyzesMultipleKotlinSourceUnitsInOneWorkerBatch() {
        val root = Files.createTempDirectory("kide-psi-batch-")
        Files.writeString(root.resolve("One.kt"), "class One")
        Files.writeString(root.resolve("Two.kt"), "class Two")
        val payload = buildJsonObject {
            put("source_units", buildJsonArray {
                add(sourceUnit("One.kt"))
                add(sourceUnit("Two.kt"))
            })
        }

        val result = structuralBatch(payload, root)

        assertEquals(2, result.jsonObject["snapshots"]!!.jsonArray.size)
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
