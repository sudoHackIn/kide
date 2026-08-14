package dev.kide.worker

import java.nio.file.Path
import kotlin.test.Test
import kotlin.test.assertTrue

class K2SpringCrudIntegrationTest {
    @Test
    fun resolvesSpringSymbolsUsingTheGradleClasspath() {
        val root = Path.of(System.getProperty("user.dir"), "..", "..", "fixtures", "spring-boot-crud").normalize()
        val contexts = GradleProjectImporter.kotlinCompilationContexts(root)
        val context = contexts.values.combinedForBatch()

        requireNotNull(context)
        assertTrue(context.sourceFiles.size >= 4)
        val references = K2SemanticExtractor.resolvedReferences(selectedSourceFiles = emptyList(), context)

        assertTrue(
            references.any { it.targetKey.contains("org/springframework") },
            "K2 did not resolve any Spring symbol: $references",
        )
        val mapped = JvmBytecodeExtractor.resolvedTargetIds(
            classpath = context.classpath,
            targetKeys = references.mapTo(sortedSetOf()) { it.targetKey },
        )
        assertTrue(mapped.isNotEmpty(), "No unambiguous dependency targets: ${references.map { it.targetKey }}")
        assertTrue(mapped.values.any { it.startsWith("jvm:sha256:") })
    }
}
