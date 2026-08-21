package dev.kide.worker

import kide.worker.v1.Worker
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.int
import kotlinx.serialization.json.put

/** Bridge for the first protobuf control-plane slice. Blob bytes never cross it. */
internal object ProtobufArtifactDiscoveryAdapter {
    fun descriptor(value: JsonObject): Worker.ArtifactDescriptor {
        val unit = value["source_unit"]!!.jsonObject
        val provenance = value["provenance"]!!.jsonObject
        val identity = value["resolved_identity"]?.jsonObject
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
            .apply {
                identity?.let {
                    setEcosystem(it["ecosystem"]!!.jsonPrimitive.content)
                    it["canonical_coordinate"]?.jsonPrimitive?.contentOrNull?.let(::setCanonicalCoordinate)
                    it["resolved_version"]?.jsonPrimitive?.contentOrNull?.let(::setResolvedVersion)
                }
                value["symbol_locators"]?.jsonArray?.forEach { locator ->
                    val item = locator.jsonObject
                    addSymbolLocators(Worker.SymbolLocator.newBuilder().setQualifiedName(item["qualified_name"]!!.jsonPrimitive.content).setSymbolId(item["symbol_id"]!!.jsonPrimitive.content))
                }
            }
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
        if (value.ecosystem.isNotBlank()) put("resolved_identity", buildJsonObject {
            put("ecosystem", value.ecosystem)
            if (value.hasCanonicalCoordinate()) put("canonical_coordinate", value.canonicalCoordinate)
            if (value.hasResolvedVersion()) put("resolved_version", value.resolvedVersion)
        })
        put("symbol_locators", kotlinx.serialization.json.buildJsonArray { value.symbolLocatorsList.forEach { locator ->
            add(buildJsonObject { put("qualified_name", locator.qualifiedName); put("symbol_id", locator.symbolId) })
        } })
    }
}
