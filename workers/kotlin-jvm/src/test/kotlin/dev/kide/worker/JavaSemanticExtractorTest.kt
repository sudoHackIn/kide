package dev.kide.worker

import java.nio.file.Files
import javax.tools.ToolProvider
import kotlin.test.Test
import kotlin.test.assertTrue
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put

class JavaSemanticExtractorTest {
    @Test
    fun consumesCompilationContextsProvidedByBuildResolution() {
        val root = Files.createTempDirectory("kide-java-context-")
        Files.writeString(root.resolve("settings.gradle.kts"), "rootProject.name = \"java-context\"")
        Files.writeString(root.resolve("build.gradle.kts"), "plugins { java }")

        val contexts = GradleProjectImporter.javaCompilationContexts(root).values.toList()
        assertTrue(contexts.isNotEmpty())
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
        val contexts = GradleProjectImporter.javaCompilationContexts(root).values.toList()
        val snapshots = JavaSemanticExtractor.analyze(sourceUnits, root, contexts)
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

        val incremental = JavaSemanticExtractor.analyze(listOf(sourceUnit("src/main/java/fixture/Use.java")), root, contexts).single().jsonObject
        assertTrue(incremental["references"]!!.jsonArray.isNotEmpty(), incremental.toString())
        assertTrue(incremental["calls"]!!.jsonArray.isNotEmpty(), incremental.toString())
        assertTrue(incremental["types"]!!.jsonArray.isNotEmpty(), incremental.toString())
        val incrementalTargets = (incremental["references"]!!.jsonArray + incremental["calls"]!!.jsonArray)
            .map { it.jsonObject["target"]!!.toString() }
        assertTrue(incrementalTargets.contains(api), incremental.toString())
        assertTrue(incrementalTargets.contains(apiName), incremental.toString())
    }

    @Test
    fun preservesHierarchyAcrossMavenReactorSourceContexts() {
        val root = Files.createTempDirectory("kide-java-reactor-hierarchy-")
        val api = root.resolve("api/src/main/java/fixture/Api.java")
        val implementation = root.resolve("app/src/main/java/fixture/Impl.java")
        Files.createDirectories(api.parent)
        Files.createDirectories(implementation.parent)
        Files.writeString(api, "package fixture; public interface Api {}")
        Files.writeString(implementation, "package fixture; public final class Impl implements Api {}")
        val apiClasses = Files.createTempDirectory("kide-java-reactor-api-")
        compileJava(api, apiClasses)

        val apiContext = JavaCompilationContext(
            component = "maven:api:main",
            sourceFiles = listOf(api),
            ownedSourceFiles = listOf(api),
            sourceRoots = listOf(api.parent),
            classpath = emptyList(),
            jdkHome = javaHome(),
        )
        val appContext = JavaCompilationContext(
            component = "maven:app:main",
            // Model a reactor dependency already available only as a module
            // artifact to this javac invocation.  The canonical source ID is
            // still known through the API context.
            sourceFiles = listOf(implementation),
            ownedSourceFiles = listOf(implementation),
            sourceRoots = listOf(api.parent, implementation.parent),
            classpath = listOf(apiClasses),
            jdkHome = javaHome(),
        )
        val contexts = listOf(apiContext, appContext)
        val apiUnit = sourceUnit("api/src/main/java/fixture/Api.java", apiContext.component)
        val implementationUnit = sourceUnit("app/src/main/java/fixture/Impl.java", appContext.component)
        val snapshots = JavaSemanticExtractor.analyze(listOf(apiUnit, implementationUnit), root, contexts)
            .map { it.jsonObject }
        val apiId = snapshots
            .flatMap { it["symbols"]!!.jsonArray }
            .single { it.jsonObject["qualified_name"]!!.toString().contains("fixture.Api") }
            .jsonObject["id"]!!.toString()
        val implementationSnapshot = snapshots.single {
            it["source_unit"]!!.jsonObject["path"]!!.toString().contains("Impl.java")
        }
        assertTrue(
            implementationSnapshot["hierarchy"]!!.jsonArray.any { edge ->
                edge.jsonObject["supertype"]!!.toString() == apiId
            },
            implementationSnapshot.toString(),
        )

        val incremental = JavaSemanticExtractor.analyze(listOf(implementationUnit), root, contexts)
            .single().jsonObject
        assertTrue(
            incremental["hierarchy"]!!.jsonArray.any { edge ->
                edge.jsonObject["supertype"]!!.toString() == apiId
            },
            incremental.toString(),
        )
    }

    private fun sourceUnit(path: String, component: String = "gradle:::main") = buildJsonObject {
        put("id", "$component:$path"); put("path", path); put("language", "java"); put("component", component); put("context", "sha256:test")
    }

    private fun javaHome() = java.nio.file.Path.of(System.getProperty("java.home"))

    private fun compileJava(source: java.nio.file.Path, output: java.nio.file.Path) {
        val compiler = checkNotNull(ToolProvider.getSystemJavaCompiler())
        compiler.getStandardFileManager(null, null, Charsets.UTF_8).use { fileManager ->
            val task = compiler.getTask(
                null,
                fileManager,
                null,
                listOf("-d", output.toString()),
                null,
                fileManager.getJavaFileObjectsFromPaths(listOf(source)),
            )
            assertTrue(task.call(), "compiles reactor API artifact")
        }
    }
}
