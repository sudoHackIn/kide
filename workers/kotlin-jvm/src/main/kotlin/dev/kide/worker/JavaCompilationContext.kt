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
    /** Build-derived fingerprint retained for run-local artifact hand-off. */
    val artifactContext: String = "",
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
    private val cached = mutableMapOf<Pair<Path, String>, List<JavaCompilationContext>>()

    /**
     * Build imports are expensive. Core's project context fingerprint covers
     * build configuration inputs, so it is the invalidation boundary for this
     * worker-local cache. A new worker always starts cold.
     */
    fun forWorkspace(workspaceRoot: Path, projectContext: String): List<JavaCompilationContext> {
        val workspace = workspaceRoot.toAbsolutePath().normalize()
        return cached.getOrPut(workspace to projectContext) {
            when {
                java.nio.file.Files.isRegularFile(workspace.resolve("pom.xml")) -> MavenProjectImporter.javaCompilationContexts(workspace)
                else -> GradleProjectImporter.javaCompilationContexts(workspace).values.toList()
            }
        }
    }
}
