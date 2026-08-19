package dev.kide.worker

import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertTrue
import kotlin.test.assertSame
import kotlin.test.assertNotSame
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put

class JavaSemanticExtractorTest {
    @Test
    fun cachesCompilationContextsByCoreProjectFingerprint() {
        val root = Files.createTempDirectory("kide-java-context-")
        Files.writeString(root.resolve("settings.gradle.kts"), "rootProject.name = \"java-context\"")
        Files.writeString(root.resolve("build.gradle.kts"), "plugins { java }")

        val first = JavaCompilationContexts.forWorkspace(root, "sha256:one")
        val reused = JavaCompilationContexts.forWorkspace(root, "sha256:one")
        val invalidated = JavaCompilationContexts.forWorkspace(root, "sha256:two")

        assertSame(first, reused)
        assertNotSame(first, invalidated)
    }

    @Test
    fun emitsExactJavaFactsAcrossSourceFiles() {
        val root = Files.createTempDirectory("kide-java-semantic-")
        Files.writeString(root.resolve("settings.gradle.kts"), "rootProject.name = \"java-fixture\"")
        Files.writeString(root.resolve("build.gradle.kts"), "plugins { java }")
        val sources = root.resolve("src/main/java/fixture")
        Files.createDirectories(sources)
        Files.writeString(sources.resolve("Api.java"), "package fixture; public interface Api { String name(); }")
        Files.writeString(sources.resolve("Marker.java"), "package fixture; public @interface Marker {}")
        Files.writeString(sources.resolve("Impl.java"), "package fixture; @Marker public final class Impl implements Api { public String name() { return \"ok\"; } }")
        Files.writeString(sources.resolve("Use.java"), "package fixture; public final class Use { String call(Api api) { return api.name(); } }")

        val sourceUnits = listOf("Api.java", "Marker.java", "Impl.java", "Use.java").map { file -> sourceUnit("src/main/java/fixture/$file") }
        val snapshots = JavaSemanticExtractor.analyze(sourceUnits, root)
        val allSymbols = snapshots.flatMap { it.jsonObject["symbols"]!!.jsonArray }
        assertTrue(allSymbols.any { it.jsonObject["qualified_name"]!!.toString().contains("fixture.Api") }, snapshots.toString())
        val api = allSymbols.first { it.jsonObject["qualified_name"]!!.toString().contains("fixture.Api") }.jsonObject["id"]!!.toString()
        val apiName = allSymbols.first { it.jsonObject["qualified_name"]!!.toString().contains("fixture.Api.name") }.jsonObject["id"]!!.toString()
        val marker = allSymbols.first { it.jsonObject["qualified_name"]!!.toString().contains("fixture.Marker") }.jsonObject["id"]!!.toString()
        val implementation = allSymbols.first { it.jsonObject["qualified_name"]!!.toString().contains("fixture.Impl") }.jsonObject
        val use = snapshots.single { it.jsonObject["source_unit"]!!.jsonObject["path"]!!.toString().contains("Use.java") }.jsonObject

        assertTrue(use["calls"]!!.jsonArray.isNotEmpty(), use.toString())
        assertTrue(use["references"]!!.jsonArray.isNotEmpty(), use.toString())
        assertTrue(snapshots.any { it.jsonObject["hierarchy"]!!.jsonArray.isNotEmpty() }, snapshots.toString())
        assertTrue(use["types"]!!.jsonArray.isNotEmpty(), use.toString())
        assertTrue(api.isNotBlank())
        assertTrue(implementation["applied_symbols"]!!.jsonArray.any { it.toString() == marker }, implementation.toString())

        val incremental = JavaSemanticExtractor.analyze(listOf(sourceUnit("src/main/java/fixture/Use.java")), root).single().jsonObject
        assertTrue(incremental["references"]!!.jsonArray.isNotEmpty(), incremental.toString())
        assertTrue(incremental["calls"]!!.jsonArray.isNotEmpty(), incremental.toString())
        assertTrue(incremental["types"]!!.jsonArray.isNotEmpty(), incremental.toString())
        val incrementalTargets = (incremental["references"]!!.jsonArray + incremental["calls"]!!.jsonArray)
            .map { it.jsonObject["target"]!!.toString() }
        assertTrue(incrementalTargets.contains(api), incremental.toString())
        assertTrue(incrementalTargets.contains(apiName), incremental.toString())
    }

    private fun sourceUnit(path: String) = buildJsonObject {
        put("id", "gradle:::main:$path"); put("path", path); put("language", "java"); put("component", "gradle:::"); put("context", "sha256:test")
    }
}
