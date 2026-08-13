package dev.kide.worker

import java.nio.file.Path
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

internal const val WORKER_NAME = "kide-kotlin-jvm"
internal const val WORKER_VERSION = "0.1.0"

fun main(args: Array<String>) {
    when {
        args.contentEquals(arrayOf("--handshake")) -> println(handshakeJson())
        args.contentEquals(arrayOf("--version")) -> println("$WORKER_NAME $WORKER_VERSION")
        args.isEmpty() || args.contentEquals(arrayOf("--serve")) -> serve()
        else -> error("Usage: $WORKER_NAME [--serve | --handshake | --version]")
    }
}

/** Runs one newline-delimited request/response stream for Core's supervisor. */
private fun serve() {
    System.`in`.bufferedReader().lineSequence().forEach { line ->
        val response = try {
            dispatch(protocolJson.decodeFromString<WorkerEnvelope>(line))
        } catch (error: Exception) {
            WorkerEnvelope(
                protocolVersion = WORKER_PROTOCOL_VERSION,
                requestId = "unknown",
                kind = WorkerMessageKind.ERROR,
                payload = protocolErrorPayload("invalid_request", error.message ?: error::class.simpleName.orEmpty(), false),
            )
        }
        println(protocolJson.encodeToString(response))
    }
}

internal fun dispatch(request: WorkerEnvelope): WorkerEnvelope {
    validateProtocolVersion(request.protocolVersion)?.let { error ->
        return WorkerEnvelope(
            protocolVersion = WORKER_PROTOCOL_VERSION,
            requestId = request.requestId,
            kind = WorkerMessageKind.ERROR,
            payload = protocolJson.encodeToJsonElement(WorkerProtocolError.serializer(), error),
        )
    }
    return when (request.kind) {
        WorkerMessageKind.HANDSHAKE_REQUEST -> handshakeEnvelope().copy(requestId = request.requestId)
        WorkerMessageKind.PROJECT_MANIFEST_REQUEST -> {
            val workspaceRoot = request.payload.jsonObject["workspace_root"]?.jsonPrimitive?.content
                ?: return unsupported(request.requestId, "project_manifest_request requires workspace_root")
            try {
                WorkerEnvelope(
                    protocolVersion = WORKER_PROTOCOL_VERSION,
                    requestId = request.requestId,
                    kind = WorkerMessageKind.PROJECT_MANIFEST_RESPONSE,
                    payload = buildJsonObject {
                        put("manifest", GradleProjectImporter.import(Path.of(workspaceRoot)))
                    },
                )
            } catch (error: Exception) {
                unsupported(request.requestId, error.message ?: "Gradle project import failed")
            }
        }
        else -> unsupported(request.requestId, "worker does not implement ${request.kind.name.lowercase()}")
    }
}

private fun unsupported(requestId: String, message: String): WorkerEnvelope =
    WorkerEnvelope(
        protocolVersion = WORKER_PROTOCOL_VERSION,
        requestId = requestId,
        kind = WorkerMessageKind.ERROR,
        payload = protocolErrorPayload("unsupported_capability", message, false),
    )

private fun protocolErrorPayload(code: String, message: String, retryable: Boolean) = buildJsonObject {
    put("code", code)
    put("message", message)
    put("retryable", retryable)
    put("supported_protocol_version", WORKER_PROTOCOL_VERSION)
    put("received_protocol_version", null)
}
