package dev.kide.worker

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import kotlinx.serialization.json.JsonPrimitive

class ProtobufManifestAdapterTest {
    @Test
    fun projectManifestFixtureRoundTripsThroughGeneratedTypes() {
        val fixture = requireNotNull(javaClass.classLoader.getResource("project-manifest-response.json")).readText()
        val manifest = Json.parseToJsonElement(fixture).jsonObject["payload"]!!.jsonObject["manifest"]!!.jsonObject

        assertEquals(manifest, ProtobufManifestAdapter.json(ProtobufManifestAdapter.manifest(manifest)))
    }

    @Test
    fun analyzeBatchRetainsEverySourceSnapshotIdentityField() {
        val original = buildJsonObject {
            put("workspace", "workspace"); put("project_fingerprint", "sha256:project")
            put("requested_facts", buildJsonArray { add(JsonPrimitive("symbols")); add(JsonPrimitive("references")) })
            put("source_units", buildJsonArray { add(buildJsonObject {
                put("id", "unit"); put("component", "component"); put("path", "src/Main.kt")
                put("language", "kotlin"); put("origin", "source"); put("content", "sha256:content"); put("context", "sha256:context")
            }) })
        }
        val restored = ProtobufManifestAdapter.json(ProtobufManifestAdapter.analyzeBatchRequest(original))
        val unit = restored["source_units"]!!.jsonArray.single().jsonObject
        assertEquals("sha256:content", unit["content"]!!.jsonPrimitive.content)
        assertEquals("source", unit["origin"]!!.jsonPrimitive.content)
        assertEquals("references", restored["requested_facts"]!!.jsonArray[1].jsonPrimitive.content)
    }
}
