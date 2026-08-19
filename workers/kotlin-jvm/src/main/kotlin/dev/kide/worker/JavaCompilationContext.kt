package dev.kide.worker

import java.nio.file.Path
import kotlinx.serialization.json.jsonObject

/**
 * Build-neutral inputs for one Java component. Build importers own creation of
 * this record; the shard planner and javac adapter never need Maven/Gradle
 * specific facts.
 */
internal data class JavaCompilationContext(
    val component: String,
    /** All explicit compiler inputs: owned sources plus project dependencies. */
    val sourceFiles: List<Path>,
    /** Sources whose snapshots this component is allowed to publish. */
    val ownedSourceFiles: List<Path>,
    /** On-demand search roots for non-shard local/project sources. */
    val sourceRoots: List<Path>,
    val classpath: List<Path>,
    val jdkHome: Path,
    val languageLevel: String? = null,
    val unresolvedDependencies: List<String> = emptyList(),
)

internal object JavaCompilationPlanner {
    data class Shard(
        val selected: Map<Path, kotlinx.serialization.json.JsonElement>,
        val context: JavaCompilationContext,
    )

    fun shards(
        selected: Map<Path, kotlinx.serialization.json.JsonElement>,
        contexts: List<JavaCompilationContext>,
    ): List<Shard> {
        val shards = contexts.mapNotNull { context ->
            selected.filter { (path, source) -> path in context.ownedSourceFiles || source.jsonObject.requiredString("component") == context.component }
                .takeIf { it.isNotEmpty() }
                ?.let { requested -> Shard(requested, context) }
        }
        require(shards.flatMap { it.selected.keys }.toSet() == selected.keys) {
            "Java source units do not belong to a discovered compilation context"
        }
        return shards
    }
}

internal object JavaCompilationContexts {
    fun forWorkspace(workspaceRoot: Path): List<JavaCompilationContext> = when {
        java.nio.file.Files.isRegularFile(workspaceRoot.resolve("pom.xml")) -> MavenProjectImporter.javaCompilationContexts(workspaceRoot)
        else -> GradleProjectImporter.javaCompilationContexts(workspaceRoot).values.toList()
    }
}
