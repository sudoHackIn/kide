package dev.kide.worker

import kide.worker.v1.Worker

internal const val WORKER_PROTOCOL_VERSION = 3
internal const val WORKER_NAME = "kide-kotlin-jvm"
internal const val WORKER_VERSION = "0.1.0"

internal fun handshakeEnvelope(requestId: String = "handshake"): Worker.Envelope =
    Worker.Envelope.newBuilder()
        .setProtocolVersion(WORKER_PROTOCOL_VERSION)
        .setRequestId(requestId)
        .setHandshakeResponse(
            Worker.HandshakeResponse.newBuilder()
                .setBackend(WORKER_NAME)
                .setBackendVersion(WORKER_VERSION)
                .setProtocolVersion(WORKER_PROTOCOL_VERSION)
                // Dependency descriptors may be Java bytecode, but this
                // worker accepts Kotlin source units only for analysis.
                .addLanguages("kotlin")
                .addAllCapabilities(listOf("handshake", "project_manifest", "file_analysis_snapshot", "dependency_analysis")),
        )
        .build()

internal fun protocolError(
    requestId: String,
    code: String,
    message: String,
    retryable: Boolean = false,
    receivedVersion: Int? = null,
): Worker.Envelope = Worker.Envelope.newBuilder()
    .setProtocolVersion(WORKER_PROTOCOL_VERSION)
    .setRequestId(requestId)
    .setError(
        Worker.Error.newBuilder()
            .setCode(code)
            .setMessage(message)
            .setRetryable(retryable)
            .setSupportedProtocolVersion(WORKER_PROTOCOL_VERSION)
            .apply { receivedVersion?.let(::setReceivedProtocolVersion) },
    )
    .build()
