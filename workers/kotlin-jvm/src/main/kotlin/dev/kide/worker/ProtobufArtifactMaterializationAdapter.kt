package dev.kide.worker

import com.google.protobuf.ByteString
import kide.worker.v1.Worker
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.long
import kotlinx.serialization.json.put

/** Typed staging/error adapter. Artifact payload bytes stay in the staged file. */
internal object ProtobufArtifactMaterializationAdapter {
    private fun sha256(value: String): ByteString {
        val hex = value.removePrefix("sha256:")
        require(value.startsWith("sha256:") && hex.length == 64 && hex.all { it.isDigit() || it in 'a'..'f' }) {
            "sha256 must be 32 bytes encoded as lowercase hexadecimal"
        }
        return ByteString.copyFrom(ByteArray(32) { index ->
            hex.substring(index * 2, index * 2 + 2).toInt(16).toByte()
        })
    }

    private fun sha256(value: ByteString): String {
        require(value.size() == 32) { "sha256 must contain exactly 32 bytes" }
        return "sha256:" + value.toByteArray().joinToString("") { "%02x".format(it.toInt() and 0xff) }
    }

    fun request(value: JsonObject): Worker.ArtifactMaterializationRequest {
        val stagingDirectory = value["staging_directory"]!!.jsonPrimitive.content
        val version = value["blob_format_version"]!!.jsonPrimitive.int
        require(stagingDirectory.isNotEmpty() && version > 0) { "invalid staging metadata" }
        return Worker.ArtifactMaterializationRequest.newBuilder()
            .setWorkspaceRoot(value["workspace_root"]!!.jsonPrimitive.content)
            .setArtifact(ProtobufArtifactDiscoveryAdapter.descriptor(value["artifact"]!!.jsonObject))
            .setStagingDirectory(stagingDirectory)
            .setBlobFormatVersion(version)
            .build()
    }

    fun json(value: Worker.ArtifactMaterializationRequest): JsonObject = buildJsonObject {
        require(value.hasArtifact() && value.stagingDirectory.isNotEmpty() && value.blobFormatVersion > 0) {
            "invalid staging metadata"
        }
        put("workspace_root", value.workspaceRoot)
        put("artifact", ProtobufArtifactDiscoveryAdapter.json(value.artifact))
        put("staging_directory", value.stagingDirectory)
        put("blob_format_version", value.blobFormatVersion)
    }

    fun response(value: JsonObject): Worker.ArtifactMaterializationResponse {
        val filename = value["staged_filename"]!!.jsonPrimitive.content
        val length = value["byte_length"]!!.jsonPrimitive.long
        val version = value["blob_format_version"]!!.jsonPrimitive.int
        require(filename.isNotEmpty() && length > 0 && version > 0) { "invalid completion metadata" }
        return Worker.ArtifactMaterializationResponse.newBuilder()
            .setStagedFilename(filename)
            .setByteLength(length)
            .setSha256(sha256(value["sha256"]!!.jsonPrimitive.content))
            .setBlobFormatVersion(version)
            .addAllTimings(value["timings"]?.jsonArray?.map { timing ->
                val entry = timing.jsonObject
                Worker.PhaseTiming.newBuilder().setPhase(entry["phase"]!!.jsonPrimitive.content)
                    .setElapsedMillis(entry["elapsed_millis"]!!.jsonPrimitive.long).build()
            } ?: emptyList())
            .addAllMetrics(value["metrics"]?.jsonArray?.map { metric ->
                val entry = metric.jsonObject
                Worker.WorkerMetric.newBuilder().setName(entry["name"]!!.jsonPrimitive.content)
                    .setValue(entry["value"]!!.jsonPrimitive.long).build()
            } ?: emptyList())
            .build()
    }

    fun json(value: Worker.ArtifactMaterializationResponse): JsonObject = buildJsonObject {
        require(value.stagedFilename.isNotEmpty() && value.byteLength > 0 && value.blobFormatVersion > 0) {
            "invalid completion metadata"
        }
        put("staged_filename", value.stagedFilename)
        put("byte_length", value.byteLength)
        put("sha256", sha256(value.sha256))
        put("blob_format_version", value.blobFormatVersion)
        put("timings", kotlinx.serialization.json.JsonArray(value.timingsList.map { timing -> buildJsonObject {
            put("phase", timing.phase); put("elapsed_millis", timing.elapsedMillis)
        } }))
        put("metrics", kotlinx.serialization.json.JsonArray(value.metricsList.map { metric -> buildJsonObject {
            put("name", metric.name); put("value", metric.value)
        } }))
    }

    fun error(value: JsonObject): Worker.Error {
        val code = value["code"]!!.jsonPrimitive.content
        val supportedVersion = value["supported_protocol_version"]!!.jsonPrimitive.int
        require(code in setOf("incompatible_protocol_version", "invalid_request", "unsupported_capability", "analysis_failed", "internal")) {
            "unsupported worker error code"
        }
        require(supportedVersion > 0) { "supported protocol version must be positive" }
        return Worker.Error.newBuilder()
            .setCode(code)
            .setMessage(value["message"]!!.jsonPrimitive.content)
            .setRetryable(value["retryable"]!!.jsonPrimitive.content.toBooleanStrict())
            .setSupportedProtocolVersion(supportedVersion)
            .apply { value["received_protocol_version"]?.jsonPrimitive?.contentOrNull?.toInt()?.let(::setReceivedProtocolVersion) }
            .build()
    }

    fun json(value: Worker.Error): JsonObject = buildJsonObject {
        put("code", value.code); put("message", value.message); put("retryable", value.retryable)
        put("supported_protocol_version", value.supportedProtocolVersion)
        put("received_protocol_version", if (value.hasReceivedProtocolVersion()) JsonPrimitive(value.receivedProtocolVersion) else JsonNull)
    }
}
