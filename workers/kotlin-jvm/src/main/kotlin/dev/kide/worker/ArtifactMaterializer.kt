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
        val workspace = resolveWorkspacePath(request.workspaceRoot)
        val artifact = resolvedArtifacts(workspace).singleOrNull { candidate ->
            val descriptor = JvmBytecodeExtractor.descriptor(candidate.path, candidate.component, candidate.context).jsonObject
            descriptor["source_unit"]!!.jsonObject["id"]!!.toString().trim('"') == request.artifact.sourceUnitId
        } ?: error("requested artifact is not resolved by this workspace")
        return stage(artifact.path, artifact.component, artifact.context, Path.of(request.stagingDirectory))
    }

    fun stage(artifact: Path, component: String, context: String, directory: Path): Worker.ArtifactMaterializationResponse {
        val snapshot = ProtobufAnalysisSnapshotAdapter.snapshot(JvmBytecodeExtractor.extractArtifact(artifact, component, context).jsonObject)
        val graph = graph(snapshot)
        val bytes = JvmArtifactBlobLayout.encode(graph)
        Files.createDirectories(directory)
        val staged = Files.createTempFile(directory, "kide-jvm-", ".blob")
        FileChannel.open(staged, StandardOpenOption.WRITE).use { channel ->
            channel.write(java.nio.ByteBuffer.wrap(bytes))
            channel.force(true)
        }
        return Worker.ArtifactMaterializationResponse.newBuilder()
            .setStagedFilename(staged.fileName.toString())
            .setByteLength(bytes.size.toLong())
            .setSha256(com.google.protobuf.ByteString.copyFrom(MessageDigest.getInstance("SHA-256").digest(bytes)))
            .setBlobFormatVersion(JvmArtifactBlobLayout.VERSION)
            .build()
    }

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
