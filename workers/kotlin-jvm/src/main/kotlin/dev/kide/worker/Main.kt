package dev.kide.worker

private const val WORKER_NAME = "kide-kotlin-jvm"
private const val WORKER_VERSION = "0.1.0"
private const val PROTOCOL_VERSION = 1

fun main(args: Array<String>) {
    when (args.singleOrNull()) {
        "--handshake" -> println(handshakeJson())
        "--version" -> println("$WORKER_NAME $WORKER_VERSION")
        else -> error("Usage: $WORKER_NAME --handshake | --version")
    }
}

internal fun handshakeJson(): String =
    """{"worker":"$WORKER_NAME","version":"$WORKER_VERSION","protocol_version":$PROTOCOL_VERSION,"capabilities":["handshake"]}"""
