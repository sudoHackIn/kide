package dev.kide.worker

import kide.worker.v1.Worker
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

/** Typed bridge for portable manifests and AnalyzeBatch source snapshots. */
internal object ProtobufManifestAdapter {
    fun sourceUnit(value: JsonObject): Worker.SourceUnit = Worker.SourceUnit.newBuilder()
        .setId(value["id"]!!.jsonPrimitive.content)
        .setComponent(value["component"]!!.jsonPrimitive.content)
        .setPath(value["path"]!!.jsonPrimitive.content)
        .setLanguage(value["language"]!!.jsonPrimitive.content)
        .setOrigin(value["origin"]!!.jsonPrimitive.content)
        .setContent(value["content"]!!.jsonPrimitive.content)
        .setContext(value["context"]!!.jsonPrimitive.content)
        .build()

    fun json(value: Worker.SourceUnit): JsonObject = buildJsonObject {
        put("id", value.id); put("component", value.component); put("path", value.path)
        put("language", value.language); put("origin", value.origin); put("content", value.content); put("context", value.context)
    }

    fun analyzeBatchRequest(value: JsonObject): Worker.AnalyzeBatchRequest = Worker.AnalyzeBatchRequest.newBuilder()
        .setWorkspace(value["workspace"]!!.jsonPrimitive.content)
        .setProjectFingerprint(value["project_fingerprint"]!!.jsonPrimitive.content)
        .addAllRequestedFacts(value["requested_facts"]!!.jsonArray.map { it.jsonPrimitive.content })
        .addAllSourceUnits(value["source_units"]!!.jsonArray.map { sourceUnit(it.jsonObject) })
        .build()

    fun json(value: Worker.AnalyzeBatchRequest): JsonObject = buildJsonObject {
        put("workspace", value.workspace); put("project_fingerprint", value.projectFingerprint)
        put("requested_facts", buildJsonArray { value.requestedFactsList.forEach { add(JsonPrimitive(it)) } })
        put("source_units", buildJsonArray { value.sourceUnitsList.forEach { add(json(it)) } })
    }
}
