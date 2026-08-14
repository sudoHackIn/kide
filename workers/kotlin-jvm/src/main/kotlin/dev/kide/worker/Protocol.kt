package dev.kide.worker

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

internal const val WORKER_PROTOCOL_VERSION = 3

/** The stable NDJSON envelope shared with KIDE Core. */
@Serializable
internal data class WorkerEnvelope(
    @SerialName("protocol_version") val protocolVersion: Int,
    @SerialName("request_id") val requestId: String,
    val kind: WorkerMessageKind,
    val payload: JsonElement,
)

/** All v1 messages are known even while this worker implements only handshake. */
@Serializable
internal enum class WorkerMessageKind {
    @SerialName("handshake_request")
    HANDSHAKE_REQUEST,

    @SerialName("handshake_response")
    HANDSHAKE_RESPONSE,

    @SerialName("project_manifest_request")
    PROJECT_MANIFEST_REQUEST,

    @SerialName("project_manifest_response")
    PROJECT_MANIFEST_RESPONSE,

    @SerialName("analyze_batch_request")
    ANALYZE_BATCH_REQUEST,

    @SerialName("analysis_batch_response")
    ANALYSIS_BATCH_RESPONSE,

    @SerialName("artifact_analysis_request")
    ARTIFACT_ANALYSIS_REQUEST,

    @SerialName("artifact_analysis_response")
    ARTIFACT_ANALYSIS_RESPONSE,

    @SerialName("artifact_discovery_request")
    ARTIFACT_DISCOVERY_REQUEST,

    @SerialName("artifact_discovery_response")
    ARTIFACT_DISCOVERY_RESPONSE,

    @SerialName("analysis_delta")
    ANALYSIS_DELTA,

    @SerialName("error")
    ERROR,
}

@Serializable
internal data class WorkerProtocolError(
    val code: String,
    val message: String,
    val retryable: Boolean,
    @SerialName("supported_protocol_version") val supportedProtocolVersion: Int,
    @SerialName("received_protocol_version") val receivedProtocolVersion: Int?,
)

internal val protocolJson = Json {
    encodeDefaults = true
    ignoreUnknownKeys = false
}

internal fun validateProtocolVersion(version: Int): WorkerProtocolError? =
    if (version == WORKER_PROTOCOL_VERSION) {
        null
    } else {
        WorkerProtocolError(
            code = "incompatible_protocol_version",
            message = "worker protocol version $version is incompatible; supported version is $WORKER_PROTOCOL_VERSION",
            retryable = false,
            supportedProtocolVersion = WORKER_PROTOCOL_VERSION,
            receivedProtocolVersion = version,
        )
    }

/** Does not import a project or initialize compiler/PSI analysis. */
internal fun handshakeEnvelope(): WorkerEnvelope =
    WorkerEnvelope(
        protocolVersion = WORKER_PROTOCOL_VERSION,
        requestId = "handshake",
        kind = WorkerMessageKind.HANDSHAKE_RESPONSE,
        payload = buildJsonObject {
            put("capabilities", buildJsonObject {
                put("identity", buildJsonObject {
                    put("backend", WORKER_NAME)
                    put("backend_version", WORKER_VERSION)
                })
                put("protocol_version", WORKER_PROTOCOL_VERSION)
                put("languages", buildJsonArray {
                    add(JsonPrimitive("kotlin"))
                    add(JsonPrimitive("java"))
                })
                put("capabilities", buildJsonArray {
                    add(JsonPrimitive("handshake"))
                    add(JsonPrimitive("project_manifest"))
                    add(JsonPrimitive("file_analysis_snapshot"))
                    add(JsonPrimitive("dependency_analysis"))
                })
            })
        },
    )

internal fun handshakeJson(): String = protocolJson.encodeToString(handshakeEnvelope())
