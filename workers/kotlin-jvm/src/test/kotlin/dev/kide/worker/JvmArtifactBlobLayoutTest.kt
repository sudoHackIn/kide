package dev.kide.worker

import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.io.ByteArrayInputStream
import java.util.zip.GZIPInputStream
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
        assertEquals(6, toc.sectionsCount)
        assertEquals(Artifact.ArtifactBlobSectionKind.ARTIFACT_BLOB_SECTION_KIND_SYMBOL_DICTIONARY, toc.sectionsList.first().kind)
    }

    @Test
    fun storesLocalGraphLinksAsArtifactWideOrdinals() {
        val graph = Artifact.GraphArtifact.newBuilder().addSnapshots(
            Artifact.GraphSnapshot.newBuilder()
                .addSymbols(Artifact.ArtifactSymbol.newBuilder().setId("java:fixture.Base").setName("Base"))
                .addSymbols(Artifact.ArtifactSymbol.newBuilder().setId("java:fixture.Child").setName("Child").setOwnerId("java:fixture.Base"))
                .addHierarchy(Artifact.ArtifactHierarchy.newBuilder().setSubtypeSymbolId("java:fixture.Child").setSupertypeSymbolId("java:fixture.Base"))
                .addReferences(Artifact.ArtifactReference.newBuilder().setTargetSymbolId("java:fixture.Base"))
                .build(),
        ).build()

        val blob = JvmArtifactBlobLayout.encode(graph)
        val header = ByteBuffer.wrap(blob).order(ByteOrder.LITTLE_ENDIAN).apply { position(12) }
        val tocOffset = header.long.toInt()
        val tocLength = header.long.toInt()
        val toc = Artifact.ArtifactBlobToc.parseFrom(blob.copyOfRange(tocOffset, tocOffset + tocLength))
        val facts = toc.sectionsList.single { it.kind == Artifact.ArtifactBlobSectionKind.ARTIFACT_BLOB_SECTION_KIND_GRAPH_FACTS }
        val compact = GZIPInputStream(ByteArrayInputStream(blob.copyOfRange(facts.offset.toInt(), (facts.offset + facts.length).toInt())))
            .readBytes()
        val encoded = Artifact.GraphArtifact.parseFrom(compact).snapshotsList.single()

        assertEquals(0, encoded.symbolsList[1].ownerSymbolOrdinal)
        assertEquals(false, encoded.symbolsList[1].hasOwnerId())
        assertEquals(1, encoded.hierarchyList.single().subtypeSymbolOrdinal)
        assertEquals(0, encoded.hierarchyList.single().supertypeSymbolOrdinal)
        assertEquals(0, encoded.referencesList.single().targetSymbolOrdinal)
        assertEquals("", encoded.hierarchyList.single().subtypeSymbolId)
    }
}
