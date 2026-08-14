package dev.kide.worker

import java.nio.file.Path
import kide.worker.v1.Worker
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.put
import org.slf4j.Logger
import org.slf4j.LoggerFactory

fun main(args: Array<String>) {
    configureLogging()
    when {
        args.contentEquals(arrayOf("--handshake")) -> ProtobufFraming.write(System.out, handshakeEnvelope())
        args.contentEquals(arrayOf("--version")) -> println("$WORKER_NAME $WORKER_VERSION")
        args.isEmpty() || args.contentEquals(arrayOf("--serve")) -> serve()
        else -> error("Usage: $WORKER_NAME [--serve | --handshake | --version]")
    }
}


/** Runs one framed protobuf request/response stream for Core's supervisor. */
private fun serve() {
    logger().info("Worker started; waiting for framed protobuf requests")
    while (true) {
        val request = try {
            ProtobufFraming.read(System.`in`) ?: return
        } catch (error: Exception) {
            ProtobufFraming.write(System.out, protocolError("unknown", "invalid_request", error.message ?: error::class.simpleName.orEmpty()))
            return
        }
        logger().debug("Received request {} ({})", request.requestId, request.messageCase)
        val startedAt = System.nanoTime()
        val response = try {
            dispatch(request)
        } catch (error: Exception) {
            logger().error("Request {} failed", request.requestId, error)
            protocolError(request.requestId, "invalid_request", error.message ?: error::class.simpleName.orEmpty())
        }
        logger().debug(
            "Completed request {} ({}) in {} ms",
            request.requestId,
            request.messageCase,
            (System.nanoTime() - startedAt) / 1_000_000,
        )
        ProtobufFraming.write(System.out, response)
    }
}

internal fun dispatch(request: Worker.Envelope): Worker.Envelope {
    if (request.protocolVersion != WORKER_PROTOCOL_VERSION) {
        return protocolError(request.requestId, "incompatible_protocol_version", "worker protocol version ${request.protocolVersion} is incompatible; supported version is $WORKER_PROTOCOL_VERSION", receivedVersion = request.protocolVersion)
    }
    return when (request.messageCase) {
        Worker.Envelope.MessageCase.HANDSHAKE_REQUEST -> handshakeEnvelope(request.requestId)
        Worker.Envelope.MessageCase.PROJECT_MANIFEST_REQUEST -> {
            val workspaceRoot = request.projectManifestRequest.workspaceRoot
                .takeIf(String::isNotBlank) ?: return unsupported(request.requestId, "project_manifest_request requires workspace_root")
            try {
                workerPhase("project-manifest: Gradle import")
                Worker.Envelope.newBuilder().setProtocolVersion(WORKER_PROTOCOL_VERSION).setRequestId(request.requestId)
                    .setProjectManifestResponse(Worker.ProjectManifestResponse.newBuilder().setManifest(ProtobufManifestAdapter.manifest(GradleProjectImporter.import(resolveWorkspacePath(workspaceRoot)).jsonObject))).build()
            } catch (error: Exception) {
                unsupported(request.requestId, failureMessage(error, "Gradle project import failed"))
            }
        }
        Worker.Envelope.MessageCase.ANALYZE_BATCH_REQUEST -> {
            try {
                workerPhase("analyze-batch: ${request.analyzeBatchRequest.sourceUnitsCount} source units")
                Worker.Envelope.newBuilder().setProtocolVersion(WORKER_PROTOCOL_VERSION).setRequestId(request.requestId)
                    .setAnalysisBatchResponse(ProtobufAnalysisSnapshotAdapter.analysisBatchResponse(structuralBatch(ProtobufManifestAdapter.json(request.analyzeBatchRequest), workspaceRoot()))).build()
            } catch (error: Exception) {
                unsupported(request.requestId, failureMessage(error, "Kotlin structural analysis failed"))
            }
        }
        Worker.Envelope.MessageCase.ARTIFACT_ANALYSIS_REQUEST -> {
            val artifactRequest = request.artifactAnalysisRequest
            val workspaceRoot = artifactRequest.workspaceRoot.takeIf(String::isNotBlank) ?: return unsupported(request.requestId, "artifact_analysis_request requires workspace_root")
            val maxArtifacts = artifactRequest.maxArtifacts
            if (maxArtifacts !in 1..64) return unsupported(request.requestId, "max_artifacts must be between 1 and 64")
            try {
                Worker.Envelope.newBuilder().setProtocolVersion(WORKER_PROTOCOL_VERSION).setRequestId(request.requestId)
                    .setArtifactAnalysisResponse(ProtobufAnalysisSnapshotAdapter.artifactAnalysisResponse(artifactBatch(resolveWorkspacePath(workspaceRoot), maxArtifacts, if (artifactRequest.hasCursor()) artifactRequest.cursor else null))).build()
            } catch (error: Exception) {
                unsupported(request.requestId, failureMessage(error, "JVM dependency analysis failed"))
            }
        }
        Worker.Envelope.MessageCase.ARTIFACT_DISCOVERY_REQUEST -> {
            val discoveryRequest = request.artifactDiscoveryRequest
            val workspaceRoot = discoveryRequest.workspaceRoot.takeIf(String::isNotBlank) ?: return unsupported(request.requestId, "artifact_discovery_request requires workspace_root")
            val maxArtifacts = discoveryRequest.maxArtifacts
            if (maxArtifacts !in 1..64) return unsupported(request.requestId, "max_artifacts must be between 1 and 64")
            try {
                val json = artifactDescriptors(resolveWorkspacePath(workspaceRoot), maxArtifacts, if (discoveryRequest.hasCursor()) discoveryRequest.cursor else null)
                Worker.Envelope.newBuilder().setProtocolVersion(WORKER_PROTOCOL_VERSION).setRequestId(request.requestId)
                    .setArtifactDiscoveryResponse(Worker.ArtifactDiscoveryResponse.newBuilder().addAllArtifacts(json["artifacts"]!!.jsonArray.map { ProtobufArtifactDiscoveryAdapter.descriptor(it.jsonObject) }).apply { json["next_cursor"]?.jsonPrimitive?.contentOrNull?.let(::setNextCursor) }).build()
            } catch (error: Exception) {
                unsupported(request.requestId, failureMessage(error, "JVM dependency discovery failed"))
            }
        }
        Worker.Envelope.MessageCase.ARTIFACT_MATERIALIZATION_REQUEST -> {
            try {
                Worker.Envelope.newBuilder().setProtocolVersion(WORKER_PROTOCOL_VERSION).setRequestId(request.requestId)
                    .setArtifactMaterializationResponse(ArtifactMaterializer.materialize(request.artifactMaterializationRequest)).build()
            } catch (error: Exception) {
                unsupported(request.requestId, failureMessage(error, "JVM artifact materialization failed"))
            }
        }
        else -> unsupported(request.requestId, "worker does not implement ${request.messageCase.name.lowercase()}")
    }
}

private fun workspaceRoot(): Path = System.getenv("KIDE_WORKSPACE_ROOT")
    ?.takeIf(String::isNotBlank)
    ?.let(Path::of)
    ?.toAbsolutePath()
    ?.normalize()
    ?: Path.of(".").toAbsolutePath().normalize()

internal fun resolveWorkspacePath(value: String): Path {
    val path = Path.of(value)
    return if (path.isAbsolute) path else workspaceRoot().resolve(path).normalize()
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

internal fun artifactDescriptors(workspaceRoot: Path, maxArtifacts: Int, cursor: String?) = buildJsonObject {
    val artifacts = GradleProjectImporter.resolvedArtifacts(workspaceRoot)
    val start = cursor?.let { previous -> artifacts.indexOfFirst { it.cursor == previous }.takeIf { it >= 0 }?.plus(1)
        ?: error("artifact cursor is not valid for this workspace") } ?: 0
    val batch = artifacts.drop(start).take(maxArtifacts)
    put("artifacts", buildJsonArray { batch.forEach { artifact -> add(JvmBytecodeExtractor.descriptor(artifact.path, artifact.component, artifact.context)) } })
    put("next_cursor", batch.lastOrNull()?.takeIf { start + batch.size < artifacts.size }?.cursor)
}

internal fun structuralBatch(payload: kotlinx.serialization.json.JsonElement, workspaceRoot: Path) = buildJsonObject {
    val sourceUnits = payload.jsonObject["source_units"]?.jsonArray
        ?: error("analyze_batch_request requires source_units")
    require(sourceUnits.all { source -> source.jsonObject["language"]?.jsonPrimitive?.content == "kotlin" }) {
        "kide-kotlin-jvm structural worker accepts Kotlin source units only"
    }
    KotlinStructuralExtractor().use { extractor ->
        workerPhase("analyze-batch: structural extraction")
        val snapshots = sourceUnits.map { sourceUnit -> extractor.analyze(sourceUnit, workspaceRoot) }
        workerPhase("analyze-batch: Gradle compilation contexts")
        val contexts = gradleContexts(workspaceRoot)
        // Compile all requested source units in one K2 session. A Gradle module
        // dependency can be represented as sources rather than a built output;
        // the union allows cross-module resolution without keeping a backend
        // alive or materialising project artifacts.
        val context = contexts.values.combinedForBatch()
        workerPhase("analyze-batch: K2 semantic analysis")
        val facts = K2SemanticExtractor.semanticFacts(
            selectedSourceFiles = sourceUnits.map { sourceUnit -> workspaceRoot.resolve(sourceUnit.jsonObject.requiredString("path")) },
            context = context,
        )
        val externalTargets = JvmBytecodeExtractor.resolvedTargetIds(
            classpath = context?.classpath.orEmpty(),
            targetKeys = (facts.references.map { it.targetKey } + facts.hierarchy.flatMap { listOf(it.subtypeKey, it.supertypeKey) }).toSortedSet(),
        )
        workerPhase("analyze-batch: enrich snapshots")
        put("snapshots", buildJsonArray {
            K2SnapshotEnricher.enrich(snapshots, workspaceRoot, facts.references, externalTargets, facts.hierarchy).forEach(::add)
        })
    }
}

private fun workerPhase(message: String) {
    logger().debug(message)
}

private fun configureLogging() {
    val level = System.getenv("KIDE_WORKER_LOG_LEVEL")
        ?.lowercase()
        ?.takeIf { it in setOf("trace", "debug", "info", "warn", "error") }
        ?: "warn"
    // Keep third-party libraries quiet even when KIDE requests worker debug
    // logs; the useful diagnostic boundary is our own worker package.
    System.setProperty("org.slf4j.simpleLogger.defaultLogLevel", "warn")
    System.setProperty("org.slf4j.simpleLogger.log.dev.kide.worker", level)
    System.setProperty("org.slf4j.simpleLogger.logFile", "System.err")
}

private fun logger(): Logger = LoggerFactory.getLogger("dev.kide.worker")

internal fun Collection<GradleProjectImporter.KotlinCompilationContext>.combinedForBatch(): GradleProjectImporter.KotlinCompilationContext? {
    if (isEmpty()) return null
    val jdkHomes = map { it.jdkHome }.distinct()
    require(jdkHomes.size == 1) { "K2 batch spans incompatible Gradle JVM toolchains" }
    return GradleProjectImporter.KotlinCompilationContext(
        component = "k2-batch",
        sourceFiles = flatMap { it.sourceFiles }.distinct().sortedBy(Path::toString),
        classpath = flatMap { it.classpath }.distinct().sortedBy(Path::toString),
        jdkHome = jdkHomes.single(),
    )
}

private fun gradleContexts(workspaceRoot: Path): Map<String, GradleProjectImporter.KotlinCompilationContext> {
    val hasBuild = listOf("settings.gradle", "settings.gradle.kts", "build.gradle", "build.gradle.kts")
        .any { name -> workspaceRoot.resolve(name).toFile().isFile }
    return if (hasBuild) GradleProjectImporter.kotlinCompilationContexts(workspaceRoot) else emptyMap()
}

private fun failureMessage(error: Throwable, fallback: String): String = generateSequence(error) { it.cause }
    .mapNotNull { cause -> cause.message?.takeIf(String::isNotBlank) }
    .distinct()
    .joinToString("; ")
    .ifBlank { fallback }

private fun unsupported(requestId: String, message: String): Worker.Envelope =
    protocolError(requestId, "unsupported_capability", message)
