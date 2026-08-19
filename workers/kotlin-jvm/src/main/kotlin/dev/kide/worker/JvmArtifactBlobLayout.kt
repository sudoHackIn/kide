package dev.kide.worker

import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.security.MessageDigest
import java.io.ByteArrayOutputStream
import java.util.zip.GZIPOutputStream
import kide.artifact.v1.Artifact

/** Physical, section-addressable layout shared with Core's Rust reader. */
internal object JvmArtifactBlobLayout {
    const val VERSION = 1
    const val DETAIL_BLOCK_ENTRY_CAPACITY = 256
    private val magic = "KIDEJVM1".encodeToByteArray()
    private const val HEADER_SIZE = 28

    fun encode(artifact: Artifact.GraphArtifact): ByteArray {
        val graph = ordinalizeLocalGraphFacts(artifact)
        val dictionary = Artifact.ArtifactSymbolDictionary.newBuilder()
            .addAllEntries(graph.snapshotsList.flatMap { snapshot ->
                snapshot.symbolsList.map { symbol ->
                    Artifact.ArtifactSymbolDictionaryEntry.newBuilder()
                        .setId(symbol.id)
                        .setName(symbol.name)
                        .build()
                }
            })
            .build()
        val postings = Artifact.ArtifactSymbolPostings.newBuilder()
            .apply {
                var symbolOrdinal = 0
                graph.snapshotsList.forEachIndexed { sourceUnitIndex, snapshot ->
                    snapshot.symbolsList.forEach {
                        addEntries(
                            Artifact.ArtifactSymbolPosting.newBuilder()
                                .setSourceUnitIndex(sourceUnitIndex)
                                .setSymbolOrdinal(symbolOrdinal++)
                                .build(),
                        )
                    }
                }
            }
            .build()
        val hierarchyPostings = Artifact.ArtifactHierarchyPostings.newBuilder()
            .apply {
                artifact.snapshotsList.forEach { snapshot ->
                    snapshot.hierarchyList.forEach { edge ->
                        addEntries(
                            Artifact.ArtifactHierarchyPosting.newBuilder()
                                .setSubtypeSymbolId(edge.subtypeSymbolId)
                                .setSupertypeSymbolId(edge.supertypeSymbolId)
                                .setPrecision(edge.precision)
                                .build(),
                        )
                    }
                }
            }
            .build()
        val qualifiedSymbolDirectory = Artifact.ArtifactQualifiedSymbolDirectory.newBuilder()
            .addAllEntries(
                graph.snapshotsList.flatMap { it.symbolsList }
                    .mapIndexedNotNull { ordinal, symbol ->
                        symbol.takeIf { it.hasQualifiedName() }?.let {
                            Artifact.ArtifactQualifiedSymbolEntry.newBuilder()
                                .setQualifiedName(it.qualifiedName)
                                .setSymbolOrdinal(ordinal)
                                .build()
                        }
                    }
                    .sortedWith(compareBy({ it.qualifiedName }, { it.symbolOrdinal })),
            )
            .build()
        val detailSections = buildList {
            var firstOrdinal = 0
            graph.snapshotsList.forEach { snapshot ->
                if (snapshot.symbolsCount > 0) {
                    snapshot.symbolsList.chunked(DETAIL_BLOCK_ENTRY_CAPACITY).forEachIndexed { chunkIndex, symbols ->
                        fun common(value: (Artifact.ArtifactSymbol) -> String) =
                            value(symbols.first()).takeIf { candidate -> symbols.all { value(it) == candidate } } ?: ""
                        val commonProvenance = symbols.first().provenanceIndex.takeIf { candidate ->
                            symbols.all { it.hasProvenanceIndex() && it.provenanceIndex == candidate }
                        }
                        val defaults = Artifact.ArtifactSymbolSnapshotDefaults.newBuilder()
                            .setSourceUnit(snapshot.sourceUnit)
                            .addAllProvenances(snapshot.provenancesList)
                            .setLanguage(common { it.language })
                            .setFreshness(common { it.freshness })
                            .setCompleteness(common { it.completeness })
                            .setComponentId(common { it.componentId })
                            .apply { commonProvenance?.let(::setProvenanceIndex) }
                            .build()
                        val chunkStart = firstOrdinal + chunkIndex * DETAIL_BLOCK_ENTRY_CAPACITY
                        val block = Artifact.ArtifactSymbolDetailBlock.newBuilder()
                            .setFirstSymbolOrdinal(chunkStart)
                            .setDefaults(defaults)
                            .addAllEntries(symbols.map { symbol ->
                            Artifact.ArtifactSymbolDetail.newBuilder()
                                .setId(symbol.id).setBackendKey(symbol.backendKey)
                                .setBackendSchemaVersion(symbol.backendSchemaVersion)
                                .setKind(symbol.kind).setName(symbol.name)
                                .apply {
                                    if (symbol.hasQualifiedName()) setQualifiedName(symbol.qualifiedName)
                                    if (symbol.hasSignature()) setSignature(symbol.signature)
                                    if (symbol.hasDeclaration()) setDeclaration(symbol.declaration)
                                    if (symbol.hasNameRange()) setNameRange(symbol.nameRange)
                                    if (symbol.hasOwnerId()) setOwnerId(symbol.ownerId)
                                    addAllModifiers(symbol.modifiersList)
                                    addAllAppliedSymbolIds(symbol.appliedSymbolIdsList)
                                    if (symbol.language != defaults.language) setLanguage(symbol.language)
                                    if (symbol.freshness != defaults.freshness) setFreshness(symbol.freshness)
                                    if (symbol.completeness != defaults.completeness) setCompleteness(symbol.completeness)
                                    if (symbol.componentId != defaults.componentId) setComponentId(symbol.componentId)
                                    if (symbol.hasProvenanceIndex() && symbol.provenanceIndex != commonProvenance) setProvenanceIndex(symbol.provenanceIndex)
                                }.build()
                            }).build()
                        add(Section(Artifact.ArtifactBlobSectionKind.ARTIFACT_BLOB_SECTION_KIND_SYMBOL_DETAIL_BLOCK, block.toByteArray(), 0, chunkStart, chunkStart + symbols.size))
                    }
                }
                firstOrdinal += snapshot.symbolsCount
            }
        }
        val sections = listOf(
            Section(Artifact.ArtifactBlobSectionKind.ARTIFACT_BLOB_SECTION_KIND_SYMBOL_DICTIONARY, dictionary.toByteArray(), 0),
            Section(Artifact.ArtifactBlobSectionKind.ARTIFACT_BLOB_SECTION_KIND_SYMBOL_POSTINGS, postings.toByteArray(), 0),
            Section(Artifact.ArtifactBlobSectionKind.ARTIFACT_BLOB_SECTION_KIND_HIERARCHY_POSTINGS, hierarchyPostings.toByteArray(), 0),
            Section(Artifact.ArtifactBlobSectionKind.ARTIFACT_BLOB_SECTION_KIND_QUALIFIED_SYMBOL_DIRECTORY, qualifiedSymbolDirectory.toByteArray(), 0),
        ) + detailSections + Section(Artifact.ArtifactBlobSectionKind.ARTIFACT_BLOB_SECTION_KIND_GRAPH_FACTS, gzip(graph.toByteArray()), 1)
        var toc = Artifact.ArtifactBlobToc.newBuilder().setLayoutVersion(VERSION).build()
        while (true) {
            var offset = (HEADER_SIZE + toc.serializedSize).toLong()
            val next = Artifact.ArtifactBlobToc.newBuilder().setLayoutVersion(VERSION)
            sections.forEach { (kind, bytes, compression, ordinalStart, ordinalEnd) ->
                next.addSections(
                    Artifact.ArtifactBlobSection.newBuilder()
                        .setKind(kind)
                        .setOffset(offset)
                        .setLength(bytes.size.toLong())
                        .setSha256(com.google.protobuf.ByteString.copyFrom(sha256(bytes)))
                        .setCompressionValue(compression)
                        .setSymbolOrdinalStart(ordinalStart)
                        .setSymbolOrdinalEnd(ordinalEnd)
                        .build(),
                )
                offset += bytes.size
            }
            val built = next.build()
            if (built == toc) break
            toc = built
        }
        val tocBytes = toc.toByteArray()
        return ByteBuffer.allocate(HEADER_SIZE + tocBytes.size + sections.sumOf { it.bytes.size })
            .order(ByteOrder.LITTLE_ENDIAN)
            .put(magic)
            .putInt(VERSION)
            .putLong(HEADER_SIZE.toLong())
            .putLong(tocBytes.size.toLong())
            .put(tocBytes)
            .apply { sections.forEach { put(it.bytes) } }
            .array()
    }

    private fun gzip(bytes: ByteArray): ByteArray = ByteArrayOutputStream().use { output ->
        GZIPOutputStream(output).use { it.write(bytes) }
        output.toByteArray()
    }

    private fun sha256(bytes: ByteArray): ByteArray = MessageDigest.getInstance("SHA-256").digest(bytes)

    /** Replaces references to declarations within this artifact by global symbol ordinals. */
    private fun ordinalizeLocalGraphFacts(artifact: Artifact.GraphArtifact): Artifact.GraphArtifact {
        val ordinals = artifact.snapshotsList.flatMap { it.symbolsList }
            .mapIndexed { ordinal, symbol -> symbol.id to ordinal }
            .toMap()
        return Artifact.GraphArtifact.newBuilder().addAllSnapshots(artifact.snapshotsList.map { snapshot ->
            snapshot.toBuilder()
                .clearSymbols()
                .addAllSymbols(snapshot.symbolsList.map { symbol -> symbol.toBuilder().apply {
                    if (symbol.hasOwnerId()) ordinals[symbol.ownerId]?.let { ordinal ->
                        clearOwnerId(); setOwnerSymbolOrdinal(ordinal)
                    }
                }.build() })
                .clearOccurrences()
                .addAllOccurrences(snapshot.occurrencesList.map { occurrence -> occurrence.toBuilder().apply {
                    if (occurrence.hasEnclosingSymbolId()) ordinals[occurrence.enclosingSymbolId]?.let { ordinal ->
                        clearEnclosingSymbolId(); setEnclosingSymbolOrdinal(ordinal)
                    }
                    if (occurrence.hasTargetSymbolId()) ordinals[occurrence.targetSymbolId]?.let { ordinal ->
                        clearTargetSymbolId(); setTargetSymbolOrdinal(ordinal)
                    }
                }.build() })
                .clearReferences()
                .addAllReferences(snapshot.referencesList.map { edge -> edge.toBuilder().apply {
                    ordinals[edge.targetSymbolId]?.let { ordinal -> clearTargetSymbolId(); setTargetSymbolOrdinal(ordinal) }
                }.build() })
                .clearCalls()
                .addAllCalls(snapshot.callsList.map { edge -> edge.toBuilder().apply {
                    ordinals[edge.targetSymbolId]?.let { ordinal -> clearTargetSymbolId(); setTargetSymbolOrdinal(ordinal) }
                    if (edge.hasCallerSymbolId()) ordinals[edge.callerSymbolId]?.let { ordinal ->
                        clearCallerSymbolId(); setCallerSymbolOrdinal(ordinal)
                    }
                }.build() })
                .clearHierarchy()
                .addAllHierarchy(snapshot.hierarchyList.map { edge -> edge.toBuilder().apply {
                    ordinals[edge.subtypeSymbolId]?.let { ordinal -> clearSubtypeSymbolId(); setSubtypeSymbolOrdinal(ordinal) }
                    ordinals[edge.supertypeSymbolId]?.let { ordinal -> clearSupertypeSymbolId(); setSupertypeSymbolOrdinal(ordinal) }
                }.build() })
                .build()
        }).build()
    }

    private data class Section(
        val kind: Artifact.ArtifactBlobSectionKind,
        val bytes: ByteArray,
        val compression: Int,
        val ordinalStart: Int = 0,
        val ordinalEnd: Int = 0,
    )
}
