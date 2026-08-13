package dev.kide.worker

import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class K2SemanticExtractorTest {
    @Test
    fun resolvesACrossFileReferenceToItsDeclaration() {
        val root = Files.createTempDirectory("kide-k2-semantic-")
        val declaration = root.resolve("Api.kt")
        val use = root.resolve("Use.kt")
        Files.writeString(declaration, "package fixture\nclass Api\n")
        Files.writeString(use, "package fixture\nfun use() = Api()\n")

        val references = K2SemanticExtractor.resolvedReferences(selectedSourceFiles = listOf(declaration, use))
        assertTrue(references.isNotEmpty(), references.toString())

        assertTrue(references.any { reference ->
            reference.sourcePath == use.toString() && reference.startUtf16 == "package fixture\nfun use() = ".length
        }, references.toString())
        val apiReference = references.first { reference ->
            reference.sourcePath == use.toString() && reference.startUtf16 == "package fixture\nfun use() = ".length
        }
        assertEquals("callable:fixture/Api.Api", apiReference.targetKey)
        assertTrue(apiReference.isCall)
    }
}
