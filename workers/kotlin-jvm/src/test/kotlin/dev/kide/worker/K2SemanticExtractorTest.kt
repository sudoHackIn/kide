package dev.kide.worker

import java.nio.file.Files
import java.nio.file.Path
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
        assertEquals("callable:fixture/Api.Api#()", apiReference.targetKey)
        assertTrue(apiReference.isCall)
    }

    @Test
    fun resolvesKotlinReferencesToJavaSourcesInItsCompilationContext() {
        val root = Files.createTempDirectory("kide-k2-java-source-")
        val api = root.resolve("Api.java")
        val use = root.resolve("Use.kt")
        Files.writeString(api, "package fixture; public interface Api { String name(); }\n")
        Files.writeString(use, "package fixture\nfun use(api: Api) = api.name()\n")

        val facts = K2SemanticExtractor.semanticFacts(
            selectedSourceFiles = listOf(use),
            context = GradleProjectImporter.KotlinCompilationContext(
                component = "fixture",
                moduleName = "fixture",
                gradlePath = ":fixture",
                sourceFiles = listOf(api),
                classpath = emptyList(),
                jdkHome = Path.of(System.getProperty("java.home")),
            ),
        )
        assertTrue(facts.references.isNotEmpty(), facts.toString())
    }
}
