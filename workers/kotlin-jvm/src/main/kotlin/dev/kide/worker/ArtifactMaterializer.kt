package dev.kide.worker

import java.nio.file.Files
import java.nio.file.Path
import java.nio.channels.FileChannel
import java.nio.file.StandardOpenOption
import java.security.MessageDigest
import kide.artifact.v1.Artifact
import kide.worker.v1.Worker
import kotlinx.serialization.json.jsonObject

/** Writes one extracted JVM artifact as a Core-owned staged binary blob. */
internal object ArtifactMaterializer {
    fun materialize(request: Worker.ArtifactMaterializationRequest): Worker.ArtifactMaterializationResponse {
        require(request.hasArtifact()) { "artifact materialization requires artifact descriptor" }
        require(request.blobFormatVersion == JvmArtifactBlobLayout.VERSION) { "unsupported artifact blob format" }
        val artifact = request.artifactLocator.takeIf { request.hasArtifactLocator() }
            ?.let { locator ->
                val path = Path.of(locator)
                require(Files.isRegularFile(path)) { "requested artifact locator is not a regular file" }
                ResolvedJvmArtifact(path, request.artifact.componentId, request.artifact.contextFingerprint)
            }
            ?: resolvedArtifacts(resolveWorkspacePath(request.workspaceRoot)).singleOrNull { candidate ->
                descriptorId(candidate) == request.artifact.sourceUnitId
            }
            ?: error("requested artifact is not resolved by this workspace")
        require(descriptorId(artifact) == request.artifact.sourceUnitId) {
            "requested artifact locator does not match descriptor identity"
        }
        return stage(artifact.path, artifact.component, artifact.context, Path.of(request.stagingDirectory))
    }

    private fun descriptorId(candidate: ResolvedJvmArtifact): String = (
        JvmBytecodeExtractor.descriptor(candidate.path, candidate.component, candidate.context).jsonObject
            ["source_unit"]!!.jsonObject["id"]!!.toString().trim('"')
        )

    fun stage(artifact: Path, component: String, context: String, directory: Path): Worker.ArtifactMaterializationResponse {
        val totalStarted = System.nanoTime()
        val extractStarted = System.nanoTime()
        val snapshot = ProtobufAnalysisSnapshotAdapter.snapshot(JvmBytecodeExtractor.extractArtifact(artifact, component, context).jsonObject)
        val extractMillis = elapsedMillis(extractStarted)
        val encodeStarted = System.nanoTime()
        val graph = graph(snapshot)
        val bytes = JvmArtifactBlobLayout.encode(graph)
        val encodeMillis = elapsedMillis(encodeStarted)
        val writeStarted = System.nanoTime()
        Files.createDirectories(directory)
        val staged = Files.createTempFile(directory, "kide-jvm-", ".blob")
        FileChannel.open(staged, StandardOpenOption.WRITE).use { channel ->
            channel.write(java.nio.ByteBuffer.wrap(bytes))
            channel.force(true)
        }
        val writeMillis = elapsedMillis(writeStarted)
        return Worker.ArtifactMaterializationResponse.newBuilder()
            .setStagedFilename(staged.fileName.toString())
            .setByteLength(bytes.size.toLong())
            .setSha256(com.google.protobuf.ByteString.copyFrom(MessageDigest.getInstance("SHA-256").digest(bytes)))
            .setBlobFormatVersion(JvmArtifactBlobLayout.VERSION)
            .addTimings(phaseTiming("artifact_extract", extractMillis))
            .addTimings(phaseTiming("artifact_encode", encodeMillis))
            .addTimings(phaseTiming("artifact_stage_write", writeMillis))
            .addTimings(phaseTiming("artifact_total", elapsedMillis(totalStarted)))
            .addMetrics(workerMetric("artifact_input_bytes", Files.size(artifact)))
            .addMetrics(workerMetric("artifact_blob_bytes", bytes.size.toLong()))
            .build()
    }

    private fun elapsedMillis(started: Long): Long = (System.nanoTime() - started) / 1_000_000

    private fun phaseTiming(phase: String, millis: Long): Worker.PhaseTiming =
        Worker.PhaseTiming.newBuilder().setPhase(phase).setElapsedMillis(millis).build()

    private fun workerMetric(name: String, value: Long): Worker.WorkerMetric =
        Worker.WorkerMetric.newBuilder().setName(name).setValue(value).build()

    private fun graph(snapshot: Worker.FileAnalysisSnapshot): Artifact.GraphArtifact =
        Artifact.GraphArtifact.newBuilder().addSnapshots(
            Artifact.GraphSnapshot.newBuilder()
                .setSourceUnit(Artifact.ArtifactSourceUnit.newBuilder()
                    .setId(snapshot.sourceUnit.id).setComponent(snapshot.sourceUnit.component).setPath(snapshot.sourceUnit.path)
                    .setLanguage(snapshot.sourceUnit.language).setOrigin(snapshot.sourceUnit.origin)
                    .setContentFingerprint(snapshot.sourceUnit.content).setContextFingerprint(snapshot.sourceUnit.context))
                .addAllProvenances(snapshot.provenancesList.map { p -> Artifact.ArtifactProvenance.newBuilder().setBackend(p.backend).setBackendVersion(p.backendVersion).setWorkerProtocolVersion(p.protocolVersion).setAnalysisOptionsFingerprint(p.analysisOptionsFingerprint).build() })
                .addAllSymbols(snapshot.symbolsList.map { s -> Artifact.ArtifactSymbol.newBuilder().setId(s.id).setBackendKey(s.backendKey).setBackendSchemaVersion(s.backendSchemaVersion).setLanguage(s.language).setKind(s.kind).setName(s.name).setComponentId(s.componentId).setFreshness(s.freshness).setCompleteness(s.completeness).setProvenanceIndex(s.provenanceIndex).apply { if (s.hasDeclaration()) setDeclaration(Artifact.ArtifactRange.newBuilder().setStart(s.declaration.start).setEnd(s.declaration.end)); if (s.hasNameRange()) setNameRange(Artifact.ArtifactRange.newBuilder().setStart(s.nameRange.start).setEnd(s.nameRange.end)); if (s.hasQualifiedName()) setQualifiedName(s.qualifiedName); if (s.hasSignature()) setSignature(s.signature); if (s.hasOwnerId()) setOwnerId(s.ownerId); addAllModifiers(s.modifiersList); addAllAppliedSymbolIds(s.appliedSymbolIdsList) }.build() })
                .addAllHierarchy(snapshot.hierarchyList.map { h -> Artifact.ArtifactHierarchy.newBuilder().setSubtypeSymbolId(h.subtypeSymbolId).setSupertypeSymbolId(h.supertypeSymbolId).setPrecision(h.precision).setProvenanceIndex(h.provenanceIndex).build() })
                .setCompleteness(snapshot.completeness).setProvenanceIndex(snapshot.provenanceIndex),
        ).build()
}
