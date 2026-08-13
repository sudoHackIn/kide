package dev.kide.worker

internal const val WORKER_NAME = "kide-kotlin-jvm"
internal const val WORKER_VERSION = "0.1.0"

fun main(args: Array<String>) {
    when (args.singleOrNull()) {
        "--handshake" -> println(handshakeJson())
        "--version" -> println("$WORKER_NAME $WORKER_VERSION")
        else -> error("Usage: $WORKER_NAME --handshake | --version")
    }
}
