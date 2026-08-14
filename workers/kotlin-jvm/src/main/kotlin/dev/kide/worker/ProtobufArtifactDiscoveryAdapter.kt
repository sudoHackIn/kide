package dev.kide.worker

import kide.worker.v1.Worker
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.int
import kotlinx.serialization.json.put

/** Bridge for the first protobuf control-plane slice. Blob bytes never cross it. */
internal object ProtobufArtifactDiscoveryAdapter {
    fun descriptor(value: JsonObject): Worker.ArtifactDescriptor {
        val unit = value["source_unit"]!!.jsonObject
        val provenance = value["provenance"]!!.jsonObject
        return Worker.ArtifactDescriptor.newBuilder()
            .setSourceUnitId(unit["id"]!!.jsonPrimitive.content)
            .setComponentId(unit["component"]!!.jsonPrimitive.content)
            .setWorkspacePath(unit["path"]!!.jsonPrimitive.content)
            .setContentFingerprint(unit["content"]!!.jsonPrimitive.content)
            .setContextFingerprint(unit["context"]!!.jsonPrimitive.content)
            .setBackend(provenance["backend"]!!.jsonPrimitive.content)
            .setBackendVersion(provenance["backend_version"]!!.jsonPrimitive.content)
            .setWorkerProtocolVersion(provenance["protocol_version"]!!.jsonPrimitive.int)
            .setAnalysisOptionsFingerprint(provenance["analysis_options"]!!.jsonPrimitive.content)
            .setLanguage(unit["language"]?.jsonPrimitive?.content ?: "java")
            .setOrigin(unit["origin"]?.jsonPrimitive?.content ?: "dependency")
            .build()
    }

    fun json(value: Worker.ArtifactDescriptor) = buildJsonObject {
        put("source_unit", buildJsonObject {
            put("id", value.sourceUnitId); put("component", value.componentId); put("path", value.workspacePath)
            put("language", value.language); put("origin", value.origin); put("content", value.contentFingerprint); put("context", value.contextFingerprint)
        })
        put("provenance", buildJsonObject {
            put("backend", value.backend); put("backend_version", value.backendVersion)
            put("protocol_version", value.workerProtocolVersion); put("analysis_options", value.analysisOptionsFingerprint)
        })
    }
}
