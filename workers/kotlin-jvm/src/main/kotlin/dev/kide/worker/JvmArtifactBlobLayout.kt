package dev.kide.worker

import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.security.MessageDigest
import kide.artifact.v1.Artifact

/** Physical, section-addressable layout shared with Core's Rust reader. */
internal object JvmArtifactBlobLayout {
    const val VERSION = 1
    private val magic = "KIDEJVM1".encodeToByteArray()
    private const val HEADER_SIZE = 28

    fun encode(artifact: Artifact.GraphArtifact): ByteArray {
        val dictionary = Artifact.ArtifactSymbolDictionary.newBuilder()
            .addAllEntries(artifact.snapshotsList.flatMap { snapshot ->
                snapshot.symbolsList.map { symbol ->
                    Artifact.ArtifactSymbolDictionaryEntry.newBuilder()
                        .setId(symbol.id)
                        .setName(symbol.name)
                        .build()
                }
            })
            .build()
        val sections = listOf(
            Artifact.ArtifactBlobSectionKind.ARTIFACT_BLOB_SECTION_KIND_SYMBOL_DICTIONARY to dictionary.toByteArray(),
            Artifact.ArtifactBlobSectionKind.ARTIFACT_BLOB_SECTION_KIND_GRAPH_FACTS to artifact.toByteArray(),
        )
        var toc = Artifact.ArtifactBlobToc.newBuilder().setLayoutVersion(VERSION).build()
        while (true) {
            var offset = (HEADER_SIZE + toc.serializedSize).toLong()
            val next = Artifact.ArtifactBlobToc.newBuilder().setLayoutVersion(VERSION)
            sections.forEach { (kind, bytes) ->
                next.addSections(
                    Artifact.ArtifactBlobSection.newBuilder()
                        .setKind(kind)
                        .setOffset(offset)
                        .setLength(bytes.size.toLong())
                        .setSha256(com.google.protobuf.ByteString.copyFrom(sha256(bytes)))
                        .build(),
                )
                offset += bytes.size
            }
            val built = next.build()
            if (built == toc) break
            toc = built
        }
        val tocBytes = toc.toByteArray()
        return ByteBuffer.allocate(HEADER_SIZE + tocBytes.size + sections.sumOf { it.second.size })
            .order(ByteOrder.LITTLE_ENDIAN)
            .put(magic)
            .putInt(VERSION)
            .putLong(HEADER_SIZE.toLong())
            .putLong(tocBytes.size.toLong())
            .put(tocBytes)
            .apply { sections.forEach { put(it.second) } }
            .array()
    }

    private fun sha256(bytes: ByteArray): ByteArray = MessageDigest.getInstance("SHA-256").digest(bytes)
}
