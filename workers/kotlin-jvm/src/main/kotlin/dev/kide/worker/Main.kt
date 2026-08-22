package dev.kide.worker

import java.nio.file.Path
import com.google.protobuf.ByteString
import kide.worker.v1.Worker
import kotlinx.serialization.json.Json
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

/** Run-local resolved artifact. Its path crosses only as an opaque locator. */
internal data class ResolvedJvmArtifact(
    val path: Path,
    val component: String,
    val context: String,
    val ecosystem: String = "unknown",
    val coordinate: String? = null,
    val version: String? = null,
) {
    val cursor: String get() = path.toAbsolutePath().normalize().toString()
}

/** Backend-private compiler inputs, materialized while the build model is resolved. */
internal data class ExecutionPlan(
    val manifest: kotlinx.serialization.json.JsonObject,
    val artifacts: List<ResolvedJvmArtifact>,
    val javaContexts: List<JavaCompilationContext>,
    val kotlinContexts: Map<String, GradleProjectImporter.KotlinCompilationContext>,
)

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
                workerPhase("project-manifest: build import")
                val workspace = resolveWorkspacePath(workspaceRoot)
                val manifest = projectManifest(workspace).jsonObject
                val candidates = resolvedArtifacts(workspace)
                Worker.Envelope.newBuilder().setProtocolVersion(WORKER_PROTOCOL_VERSION).setRequestId(request.requestId)
                    .setProjectManifestResponse(
                        Worker.ProjectManifestResponse.newBuilder()
                            .setManifest(ProtobufManifestAdapter.manifest(manifest))
                            .setExecutionPlan(Worker.OpaqueExecutionPlan.newBuilder()
                                .setBackend(WORKER_NAME)
                                .setResolvedFingerprint(manifest["fingerprint"]!!.jsonPrimitive.content)
                                .setPayload(ByteString.copyFrom(encodeExecutionPlan(workspace, manifest, candidates)))),
                    )
                    .build()
            } catch (error: Exception) {
                logger().error("Build project import failed", error)
                unsupported(
                    request.requestId,
                    failureMessage(error, "build project import failed (${error::class.qualifiedName})") +
                        " at " + error.stackTrace.take(3).joinToString(" <- "),
                )
            }
        }
        Worker.Envelope.MessageCase.ANALYZE_BATCH_REQUEST -> {
            try {
                workerPhase("analyze-batch: ${request.analyzeBatchRequest.sourceUnitsCount} source units")
                require(request.analyzeBatchRequest.hasExecutionPlan()) {
                    "analyze_batch_request requires an execution plan"
                }
                require(request.analyzeBatchRequest.executionPlan.backend == WORKER_NAME) {
                    "execution plan belongs to ${request.analyzeBatchRequest.executionPlan.backend}, not $WORKER_NAME"
                }
                val startedAt = System.nanoTime()
                val plan = decodeExecutionPlan(request.analyzeBatchRequest.executionPlan.payload.toByteArray())
                require(request.analyzeBatchRequest.executionPlan.resolvedFingerprint == plan.manifest["fingerprint"]!!.jsonPrimitive.content) {
                    "execution plan fingerprint does not match its resolved manifest"
                }
                val batch = structuralBatch(ProtobufManifestAdapter.json(request.analyzeBatchRequest), workspaceRoot(), plan)
                val timed = buildJsonObject {
                    batch.forEach { (key, value) -> put(key, value) }
                    put("timings", buildJsonArray {
                        batch["timings"]?.jsonArray?.forEach(::add)
                        add(buildJsonObject {
                        put("phase", "worker_total")
                        put("elapsed_millis", (System.nanoTime() - startedAt) / 1_000_000)
                        })
                    })
                }
                val serializeStarted = System.nanoTime()
                val response = ProtobufAnalysisSnapshotAdapter.analysisBatchResponse(timed)
                val metrics = response.metricsList.toMutableList().apply {
                    add(Worker.WorkerMetric.newBuilder().setName("response_bytes").setValue(response.serializedSize.toLong()).build())
                    add(Worker.WorkerMetric.newBuilder().setName("serialize_millis").setValue((System.nanoTime() - serializeStarted) / 1_000_000).build())
                }
                Worker.Envelope.newBuilder().setProtocolVersion(WORKER_PROTOCOL_VERSION).setRequestId(request.requestId)
                    .setAnalysisBatchResponse(response.toBuilder().clearMetrics().addAllMetrics(metrics).build()).build()
            } catch (error: Exception) {
                logger().error("Source analysis failed for request {}", request.requestId, error)
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
                val json = artifactDescriptors(
                    resolveWorkspacePath(workspaceRoot),
                    maxArtifacts,
                    if (discoveryRequest.hasCursor()) discoveryRequest.cursor else null,
                    decodeExecutionPlan(discoveryRequest.executionPlan.payload.toByteArray()).artifacts,
                )
                Worker.Envelope.newBuilder().setProtocolVersion(WORKER_PROTOCOL_VERSION).setRequestId(request.requestId)
                    .setArtifactDiscoveryResponse(Worker.ArtifactDiscoveryResponse.newBuilder()
                        .addAllArtifacts(json["artifacts"]!!.jsonArray.map { ProtobufArtifactDiscoveryAdapter.descriptor(it.jsonObject) })
                        .addAllArtifactLocators(json["artifact_locators"]!!.jsonArray.map { locator ->
                            val entry = locator.jsonObject
                            Worker.ArtifactLocator.newBuilder().setSourceUnitId(entry["source_unit_id"]!!.jsonPrimitive.content)
                                .setLocator(entry["locator"]!!.jsonPrimitive.content).build()
                        })
                        .apply { json["next_cursor"]?.jsonPrimitive?.contentOrNull?.let(::setNextCursor) }).build()
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
        Worker.Envelope.MessageCase.SEMANTIC_QUERY_REQUEST -> {
            val query = request.semanticQueryRequest
            Worker.Envelope.newBuilder()
                .setProtocolVersion(WORKER_PROTOCOL_VERSION)
                .setRequestId(request.requestId)
                .setSemanticQueryResponse(
                    Worker.SemanticQueryResponse.newBuilder()
                        .setCapabilityName(query.capabilityName)
                        .setCapabilityVersion(query.capabilityVersion)
                        .setState("unsupported")
                        .setProvenance(
                            Worker.Provenance.newBuilder()
                                .setBackend(WORKER_NAME)
                                .setBackendVersion(WORKER_VERSION)
                                .setProtocolVersion(WORKER_PROTOCOL_VERSION)
                                .setAnalysisOptionsFingerprint("sha256:semantic-query-unavailable"),
                        ),
                )
                .build()
        }
        else -> unsupported(request.requestId, "worker does not implement ${request.messageCase.name.lowercase()}")
    }
}

private fun projectManifest(workspace: Path) = when {
    workspace.resolve("pom.xml").toFile().isFile -> MavenProjectImporter.import(workspace)
    else -> GradleProjectImporter.import(workspace)
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
    val artifacts = resolvedArtifacts(workspaceRoot)
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

internal fun artifactDescriptors(
    workspaceRoot: Path,
    maxArtifacts: Int,
    cursor: String?,
    artifactCandidates: List<ResolvedJvmArtifact> = emptyList(),
) = buildJsonObject {
    val artifacts = artifactCandidates.ifEmpty { resolvedArtifacts(workspaceRoot) }
    val start = cursor?.let { previous -> artifacts.indexOfFirst { it.cursor == previous }.takeIf { it >= 0 }?.plus(1)
        ?: error("artifact cursor is not valid for this workspace") } ?: 0
    val batch = artifacts.drop(start).take(maxArtifacts)
    put("artifacts", buildJsonArray { batch.forEach { artifact -> add(JvmBytecodeExtractor.descriptor(artifact.path, artifact.component, artifact.context, artifact.ecosystem, artifact.coordinate, artifact.version)) } })
    put("artifact_locators", buildJsonArray { batch.forEach { artifact ->
        val descriptor = JvmBytecodeExtractor.descriptor(artifact.path, artifact.component, artifact.context, artifact.ecosystem, artifact.coordinate, artifact.version).jsonObject
        add(buildJsonObject { put("source_unit_id", descriptor["source_unit"]!!.jsonObject["id"]!!.jsonPrimitive.content); put("locator", artifact.path.toString()) })
    } })
    put("next_cursor", batch.lastOrNull()?.takeIf { start + batch.size < artifacts.size }?.cursor)
}

internal fun resolvedArtifacts(workspace: Path): List<ResolvedJvmArtifact> = when {
    workspace.resolve("pom.xml").toFile().isFile -> MavenProjectImporter.resolvedArtifacts(workspace)
        .map { ResolvedJvmArtifact(it.path, it.component, it.context, "maven", it.coordinate, it.version) }
    else -> GradleProjectImporter.resolvedArtifacts(workspace)
        .map { ResolvedJvmArtifact(it.path, it.component, it.context) }
}

private fun encodeExecutionPlan(
    workspace: Path,
    manifest: kotlinx.serialization.json.JsonObject,
    artifacts: List<ResolvedJvmArtifact>,
): ByteArray {
    val (javaContexts, kotlinContexts) = sourceExecutionContexts(workspace)
    return buildJsonObject {
        put("format", 1)
        put("manifest", manifest)
        put("artifacts", buildJsonArray {
            artifacts.forEach { artifact -> add(buildJsonObject {
                put("path", artifact.path.toString()); put("component", artifact.component); put("context", artifact.context)
                put("ecosystem", artifact.ecosystem)
                artifact.coordinate?.let { put("coordinate", it) }
                artifact.version?.let { put("version", it) }
            }) }
        })
        put("java_contexts", buildJsonArray {
            javaContexts.forEach { context -> add(buildJsonObject {
                put("component", context.component)
                put("source_files", context.sourceFiles.jsonPaths())
                put("owned_source_files", context.ownedSourceFiles.jsonPaths())
                put("source_roots", context.sourceRoots.jsonPaths())
                put("classpath", context.classpath.jsonPaths())
                put("jdk_home", context.jdkHome.toString())
                context.languageLevel?.let { put("language_level", it) }
                put("unresolved_dependencies", context.unresolvedDependencies.jsonStrings())
                put("artifact_context", context.artifactContext)
            }) }
        })
        put("kotlin_contexts", buildJsonArray {
            kotlinContexts.values.sortedBy { it.component }.forEach { context -> add(buildJsonObject {
                put("component", context.component); put("module_name", context.moduleName); put("gradle_path", context.gradlePath)
                put("source_files", context.sourceFiles.jsonPaths()); put("classpath", context.classpath.jsonPaths()); put("jdk_home", context.jdkHome.toString())
                put("project_dependencies", context.projectDependencyModuleNames.sorted().jsonStrings())
            }) }
        })
    }.toString().encodeToByteArray()
}

private fun decodeExecutionPlan(payload: ByteArray): ExecutionPlan {
    val json = Json.parseToJsonElement(payload.decodeToString()).jsonObject
    fun kotlinx.serialization.json.JsonObject.paths(name: String) = getValue(name).jsonArray.map { Path.of(it.jsonPrimitive.content) }
    fun kotlinx.serialization.json.JsonObject.strings(name: String) = getValue(name).jsonArray.map { it.jsonPrimitive.content }
    val artifacts = json["artifacts"]!!.jsonArray.map { value ->
        val artifact = value.jsonObject
        ResolvedJvmArtifact(
            Path.of(artifact["path"]!!.jsonPrimitive.content),
            artifact["component"]!!.jsonPrimitive.content,
            artifact["context"]!!.jsonPrimitive.content,
            artifact["ecosystem"]?.jsonPrimitive?.contentOrNull ?: "unknown",
            artifact["coordinate"]?.jsonPrimitive?.contentOrNull,
            artifact["version"]?.jsonPrimitive?.contentOrNull,
        )
    }
    val javaContexts = json["java_contexts"]!!.jsonArray.map { value -> value.jsonObject.let { context ->
        JavaCompilationContext(context["component"]!!.jsonPrimitive.content, context.paths("source_files"), context.paths("owned_source_files"), context.paths("source_roots"), context.paths("classpath"), Path.of(context["jdk_home"]!!.jsonPrimitive.content), context["language_level"]?.jsonPrimitive?.contentOrNull, context.strings("unresolved_dependencies"), context["artifact_context"]!!.jsonPrimitive.content)
    } }
    val kotlinContexts = json["kotlin_contexts"]!!.jsonArray.map { value -> value.jsonObject.let { context ->
        GradleProjectImporter.KotlinCompilationContext(context["component"]!!.jsonPrimitive.content, context["module_name"]!!.jsonPrimitive.content, context["gradle_path"]!!.jsonPrimitive.content, context.paths("source_files"), context.paths("classpath"), Path.of(context["jdk_home"]!!.jsonPrimitive.content), context.strings("project_dependencies").toSet())
    } }.associateBy { it.component }
    return ExecutionPlan(json["manifest"]!!.jsonObject, artifacts, javaContexts, kotlinContexts)
}

private fun List<Path>.jsonPaths() = buildJsonArray { forEach { add(kotlinx.serialization.json.JsonPrimitive(it.toString())) } }
private fun List<String>.jsonStrings() = buildJsonArray { forEach { add(kotlinx.serialization.json.JsonPrimitive(it)) } }

private fun sourceExecutionContexts(workspace: Path): Pair<List<JavaCompilationContext>, Map<String, GradleProjectImporter.KotlinCompilationContext>> = when {
    workspace.resolve("pom.xml").toFile().isFile -> MavenProjectImporter.javaCompilationContexts(workspace) to emptyMap()
    else -> GradleProjectImporter.javaCompilationContexts(workspace).values.toList() to GradleProjectImporter.kotlinCompilationContexts(workspace)
}

internal fun structuralBatch(payload: kotlinx.serialization.json.JsonElement, workspaceRoot: Path, plan: ExecutionPlan): kotlinx.serialization.json.JsonObject {
    val sourceUnits = payload.jsonObject["source_units"]?.jsonArray
        ?: error("analyze_batch_request requires source_units")
    val language = sourceUnits.firstOrNull()?.jsonObject?.get("language")?.jsonPrimitive?.content
        ?: error("analyze_batch_request requires at least one source unit")
    require(sourceUnits.all { source -> source.jsonObject["language"]?.jsonPrimitive?.content == language }) {
        "worker batches must contain exactly one source language"
    }
    if (language == "java") return buildJsonObject {
        put("snapshots", buildJsonArray { JavaSemanticExtractor.analyze(sourceUnits, workspaceRoot, plan.javaContexts).forEach(::add) })
        put("timings", buildJsonArray { JavaSemanticExtractor.consumeTimings().forEach { (phase, elapsed) -> add(buildJsonObject { put("phase", phase); put("elapsed_millis", elapsed) }) } })
        put("artifact_candidates", buildJsonArray { JavaSemanticExtractor.artifactCandidates().forEach { candidate ->
            add(buildJsonObject { put("locator", candidate.path.toString()); put("component", candidate.component); put("context", candidate.context) })
        } })
        put("metrics", buildJsonArray { JavaSemanticExtractor.consumeMetrics().forEach { (name, value) ->
            add(buildJsonObject { put("name", name); put("value", value) })
        } })
    }
    require(language == "kotlin") { "kide-kotlin-jvm does not support $language source units" }
    return buildJsonObject { KotlinStructuralExtractor().use { extractor ->
        workerPhase("analyze-batch: structural extraction")
        val snapshots = sourceUnits.map { sourceUnit -> extractor.analyze(sourceUnit, workspaceRoot) }
        workerPhase("analyze-batch: Gradle compilation contexts")
        val contexts = plan.kotlinContexts
        workerPhase("analyze-batch: K2 semantic analysis")
        // Each Gradle component is compiled with its transitive project-source
        // dependencies. A workspace-wide K2 session incorrectly merges
        // independent Gradle projects, which may legitimately reuse packages
        // and type names (for example, separate examples).
        val analyses = sourceUnits.groupBy { it.jsonObject.requiredString("component") }
            .toSortedMap()
            .map { (component, units) ->
                val context = contexts.contextFor(component)
                context to K2SemanticExtractor.semanticFacts(
                    selectedSourceFiles = units.map { sourceUnit -> workspaceRoot.resolve(sourceUnit.jsonObject.requiredString("path")) },
                    context = context,
                )
            }
        val facts = K2SemanticFacts(
            references = analyses.flatMap { it.second.references },
            hierarchy = analyses.flatMap { it.second.hierarchy },
            annotations = analyses.flatMap { it.second.annotations },
        )
        val externalTargets = JvmBytecodeExtractor.resolvedTargetIds(
            classpath = analyses.flatMap { it.first?.classpath.orEmpty() }.distinct(),
            targetKeys = (facts.references.map { it.targetKey } + facts.hierarchy.flatMap { listOf(it.subtypeKey, it.supertypeKey) } + facts.annotations.map { it.targetKey }).toSortedSet(),
        )
        workerPhase("analyze-batch: enrich snapshots")
        put("snapshots", buildJsonArray {
            K2SnapshotEnricher.enrich(snapshots, workspaceRoot, facts.references, externalTargets, facts.hierarchy, facts.annotations).forEach(::add)
        })
    } }
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
        moduleName = "k2-batch",
        gradlePath = ":k2-batch",
        sourceFiles = flatMap { it.sourceFiles }.distinct().sortedBy(Path::toString),
        classpath = flatMap { it.classpath }.distinct().sortedBy(Path::toString),
        jdkHome = jdkHomes.single(),
    )
}

private fun Map<String, GradleProjectImporter.KotlinCompilationContext>.contextFor(
    component: String,
): GradleProjectImporter.KotlinCompilationContext? {
    val root = this[component]
        // Core canonicalizes the Gradle root separator in source-unit IDs
        // (`gradle:app:main`), while the Tooling API preserves the leading
        // Gradle path colon (`gradle::app:main`). Keep this compatibility
        // boundary local to worker-only context lookup.
        ?: values.singleOrNull { it.component.gradleComponentIdentity() == component.gradleComponentIdentity() }
        ?: return null
    val byModuleName = values.associateBy { it.moduleName }
    val included = linkedSetOf<GradleProjectImporter.KotlinCompilationContext>()
    val pending = ArrayDeque<GradleProjectImporter.KotlinCompilationContext>()
    pending += root
    while (pending.isNotEmpty()) {
        val context = pending.removeFirst()
        if (!included.add(context)) continue
        context.projectDependencyModuleNames.sorted().forEach { dependency ->
            byModuleName[dependency]?.let(pending::add)
        }
    }
    val sourceContext = included.combinedForBatch() ?: return null
    // Gradle's IDEA model may attach resolved binary libraries to sibling
    // modules rather than to the module that declares a project dependency.
    // Retain the full imported binary classpath, but never its sources: source
    // isolation above is what prevents independent projects from colliding.
    return sourceContext.copy(
        classpath = values.flatMap { it.classpath }.distinct().sortedBy(Path::toString),
    )
}

private fun String.gradleComponentIdentity(): String {
    if (!startsWith("gradle:")) return this
    val roleStart = lastIndexOf(":main")
    if (roleStart < 0) return this
    val projectPath = substring("gradle:".length, roleStart)
        .removePrefix(":")
        .replace(':', '/')
    return "gradle:$projectPath:main"
}

private fun failureMessage(error: Throwable, fallback: String): String = generateSequence(error) { it.cause }
    .mapNotNull { cause -> cause.message?.takeIf(String::isNotBlank) }
    .distinct()
    .joinToString("; ")
    .ifBlank { fallback }

private fun unsupported(requestId: String, message: String): Worker.Envelope =
    protocolError(requestId, "unsupported_capability", message)
