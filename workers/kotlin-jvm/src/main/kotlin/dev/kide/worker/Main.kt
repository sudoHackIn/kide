package dev.kide.worker

import java.nio.file.Path
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
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
                unsupported(request.requestId, failureMessage(error, "Gradle project import failed"))
            }
        }
        WorkerMessageKind.ANALYZE_BATCH_REQUEST -> {
            try {
                WorkerEnvelope(
                    protocolVersion = WORKER_PROTOCOL_VERSION,
                    requestId = request.requestId,
                    kind = WorkerMessageKind.ANALYSIS_BATCH_RESPONSE,
                    payload = structuralBatch(request.payload, Path.of(".")),
                )
            } catch (error: Exception) {
                unsupported(request.requestId, failureMessage(error, "Kotlin structural analysis failed"))
            }
        }
        WorkerMessageKind.ARTIFACT_ANALYSIS_REQUEST -> {
            val workspaceRoot = request.payload.jsonObject["workspace_root"]?.jsonPrimitive?.content
                ?: return unsupported(request.requestId, "artifact_analysis_request requires workspace_root")
            val maxArtifacts = request.payload.jsonObject["max_artifacts"]?.jsonPrimitive?.intOrNull
                ?: return unsupported(request.requestId, "artifact_analysis_request requires max_artifacts")
            if (maxArtifacts !in 1..64) return unsupported(request.requestId, "max_artifacts must be between 1 and 64")
            val cursor = request.payload.jsonObject["cursor"]?.jsonPrimitive?.contentOrNull
            try {
                WorkerEnvelope(
                    protocolVersion = WORKER_PROTOCOL_VERSION,
                    requestId = request.requestId,
                    kind = WorkerMessageKind.ARTIFACT_ANALYSIS_RESPONSE,
                    payload = artifactBatch(Path.of(workspaceRoot), maxArtifacts, cursor),
                )
            } catch (error: Exception) {
                unsupported(request.requestId, failureMessage(error, "JVM dependency analysis failed"))
            }
        }
        else -> unsupported(request.requestId, "worker does not implement ${request.kind.name.lowercase()}")
    }
}

internal fun artifactBatch(workspaceRoot: Path, maxArtifacts: Int, cursor: String?) = buildJsonObject {
    // The Gradle model is the authority for every binary artifact, including
    // platform libraries. This worker never scans a JDK installation itself.
    val artifacts = GradleProjectImporter.resolvedArtifacts(workspaceRoot)
    val start = cursor?.let { previous ->
        artifacts.indexOfFirst { it.cursor == previous }
            .takeIf { it >= 0 }
            ?.plus(1)
            ?: error("artifact cursor is not valid for this workspace")
    } ?: 0
    val batch = artifacts.drop(start).take(maxArtifacts)
    put("snapshots", buildJsonArray {
        batch.forEach { artifact ->
            JvmBytecodeExtractor.extract(artifact.path, artifact.component, artifact.context).forEach(::add)
        }
    })
    put("next_cursor", batch.lastOrNull()?.takeIf { start + batch.size < artifacts.size }?.cursor)
}

internal fun structuralBatch(payload: kotlinx.serialization.json.JsonElement, workspaceRoot: Path) = buildJsonObject {
    val sourceUnits = payload.jsonObject["source_units"]?.jsonArray
        ?: error("analyze_batch_request requires source_units")
    require(sourceUnits.all { source -> source.jsonObject["language"]?.jsonPrimitive?.content == "kotlin" }) {
        "kide-kotlin-jvm structural worker accepts Kotlin source units only"
    }
    KotlinStructuralExtractor().use { extractor ->
        put("snapshots", buildJsonArray {
            sourceUnits.forEach { sourceUnit -> add(extractor.analyze(sourceUnit, workspaceRoot)) }
        })
    }
}

private fun failureMessage(error: Throwable, fallback: String): String = generateSequence(error) { it.cause }
    .mapNotNull { cause -> cause.message?.takeIf(String::isNotBlank) }
    .distinct()
    .joinToString("; ")
    .ifBlank { fallback }

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
