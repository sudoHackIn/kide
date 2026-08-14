package dev.kide.worker

import java.nio.file.Path
import kotlin.test.Test
import kotlin.test.assertTrue
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put

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

    @Test
    fun resolvesCrossModuleSourceConstructorInOneColdBatch() {
        val root = Path.of(System.getProperty("user.dir"), "..", "..", "fixtures", "spring-boot-crud").normalize()
        val payload = buildJsonObject {
            put("source_units", buildJsonArray {
                add(sourceUnit("gradle::domain:main", "domain/src/main/kotlin/dev/kide/fixture/domain/Book.kt"))
                add(sourceUnit("gradle::app:main", "app/src/main/kotlin/dev/kide/fixture/book/BookSummary.kt"))
            })
        }

        val snapshots = structuralBatch(payload, root).jsonObject["snapshots"]!!.jsonArray.map { it.jsonObject }
        val domain = snapshots.single { it["source_unit"]!!.jsonObject["path"]!!.toString().contains("domain/Book.kt") }
        val app = snapshots.single { it["source_unit"]!!.jsonObject["path"]!!.toString().contains("BookSummary.kt") }
        val constructor = domain["symbols"]!!.jsonArray
            .map { it.jsonObject }
            .single { it["kind"]!!.toString().contains("constructor") }["id"]!!.toString()

        assertTrue(
            app["calls"]!!.jsonArray.any { call -> call.jsonObject["target"]!!.toString() == constructor },
            "Expected $constructor in ${app["calls"]}",
        )
    }

    @Test
    fun persistsExactKotlinToJavaDependencyReference() {
        val root = Path.of(System.getProperty("user.dir"), "..", "..", "fixtures", "spring-boot-crud").normalize()
        val payload = buildJsonObject {
            put("source_units", buildJsonArray {
                add(sourceUnit("gradle::app:main", "app/src/main/kotlin/dev/kide/fixture/book/BookController.kt"))
            })
        }

        val snapshot = structuralBatch(payload, root).jsonObject["snapshots"]!!.jsonArray.single().jsonObject
        val references = snapshot["references"]!!.jsonArray.map { it.jsonObject }
        assertTrue(
            references.any { reference -> reference["target"]!!.toString().contains("jvm:sha256:") },
            "No persisted exact Java dependency references: $references",
        )
    }

    private fun sourceUnit(component: String, path: String) = buildJsonObject {
        put("id", "$component:$path")
        put("component", component)
        put("path", path)
        put("language", "kotlin")
        put("origin", "workspace")
        put("content", "fixture")
        put("context", "fixture")
    }
}
