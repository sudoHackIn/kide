package dev.kide.worker

import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlin.test.Test
import kotlin.test.assertEquals
import kide.artifact.v1.Artifact

class JvmArtifactBlobLayoutTest {
    @Test
    fun writesTheSharedFixedHeaderAndSectionToc() {
        val graph = Artifact.GraphArtifact.newBuilder()
            .addSnapshots(Artifact.GraphSnapshot.newBuilder().addSymbols(
                Artifact.ArtifactSymbol.newBuilder().setId("java:example.Widget").setName("Widget"),
            ))
            .build()

        val blob = JvmArtifactBlobLayout.encode(graph)
        val header = ByteBuffer.wrap(blob).order(ByteOrder.LITTLE_ENDIAN)
        val magic = ByteArray(8).also(header::get).decodeToString()
        assertEquals("KIDEJVM1", magic)
        assertEquals(JvmArtifactBlobLayout.VERSION, header.int)
        val tocOffset = header.long
        val tocLength = header.long
        val toc = Artifact.ArtifactBlobToc.parseFrom(blob.copyOfRange(tocOffset.toInt(), (tocOffset + tocLength).toInt()))
        assertEquals(2, toc.sectionsCount)
        assertEquals(Artifact.ArtifactBlobSectionKind.ARTIFACT_BLOB_SECTION_KIND_SYMBOL_DICTIONARY, toc.sectionsList.first().kind)
    }
}
