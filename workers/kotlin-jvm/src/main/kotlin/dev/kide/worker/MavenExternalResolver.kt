package dev.kide.worker

import java.nio.file.Path
import org.apache.maven.repository.internal.MavenRepositorySystemUtils
import org.eclipse.aether.DefaultRepositorySystemSession
import org.eclipse.aether.RepositorySystem
import org.eclipse.aether.artifact.DefaultArtifact
import org.eclipse.aether.collection.CollectRequest
import org.eclipse.aether.connector.basic.BasicRepositoryConnectorFactory
import org.eclipse.aether.graph.Dependency
import org.eclipse.aether.graph.DependencyNode
import org.eclipse.aether.impl.DefaultServiceLocator
import org.eclipse.aether.repository.LocalRepository
import org.eclipse.aether.repository.RemoteRepository
import org.eclipse.aether.resolution.DependencyRequest
import org.eclipse.aether.spi.connector.RepositoryConnectorFactory
import org.eclipse.aether.spi.connector.transport.TransporterFactory
import org.eclipse.aether.transport.http.HttpTransporterFactory

/** Official Maven Resolver adapter for external artifacts only. */
internal object MavenExternalResolver {
    data class ResolvedArtifact(val path: Path, val coordinate: String, val version: String)
    data class Resolution(
        val artifacts: List<ResolvedArtifact>,
        val dependencyEdges: List<String>,
        val unresolvedCoordinates: List<String>,
    ) {
        val paths: List<Path> get() = artifacts.map(ResolvedArtifact::path)
    }
    private val repositorySystem: RepositorySystem by lazy {
        MavenRepositorySystemUtils.newServiceLocator().also { locator ->
            locator.addService(RepositoryConnectorFactory::class.java, BasicRepositoryConnectorFactory::class.java)
            locator.addService(TransporterFactory::class.java, HttpTransporterFactory::class.java)
        }.getService(RepositorySystem::class.java)
    }

    /**
     * Local-only escape hatch for opening a project whose private repository is
     * unavailable. Callers must mark their facts partial when this is enabled.
     */
    val bestEffortEnabled: Boolean get() = System.getenv("KIDE_MAVEN_BEST_EFFORT") == "1"

    fun resolve(model: org.apache.maven.model.Model, reactorCoordinates: Set<String>): List<Path> =
        resolveWithDiagnostics(model, reactorCoordinates).paths

    fun resolveWithDiagnostics(model: org.apache.maven.model.Model, reactorCoordinates: Set<String>): Resolution {
        val session: DefaultRepositorySystemSession = MavenRepositorySystemUtils.newSession()
        val local = LocalRepository(Path.of(System.getProperty("user.home"), ".m2", "repository").toFile())
        session.localRepositoryManager = repositorySystem.newLocalRepositoryManager(session, local)
        val dependencies = model.dependencies.orEmpty()
            .filter { dependency -> "${dependency.groupId}:${dependency.artifactId}" !in reactorCoordinates }
            .filter { dependency -> dependency.scope != "import" && dependency.optional != "true" }
            .map { dependency ->
                Dependency(
                    DefaultArtifact(dependency.groupId, dependency.artifactId, dependency.type ?: "jar", dependency.version),
                    dependency.scope ?: "compile",
                )
            }
        fun resolve(dependencies: List<Dependency>): Resolution {
            val request = CollectRequest().apply {
                repositories = listOf(RemoteRepository.Builder("central", "default", "https://repo.maven.apache.org/maven2/").build())
                dependencies.forEach(::addDependency)
            }
            val result = repositorySystem.resolveDependencies(session, DependencyRequest(request, null))
            val artifacts = result.artifactResults
                .mapNotNull { result -> result.artifact?.let { artifact -> artifact.file?.toPath()?.let { path -> artifact to path } } }
                .filter { (_, path) -> path.fileName.toString().endsWith(".jar") }
                .map { (artifact, path) ->
                    val classifier = artifact.classifier.takeIf(String::isNotBlank)?.let { ":$it" }.orEmpty()
                    ResolvedArtifact(path, "${artifact.groupId}:${artifact.artifactId}$classifier:${artifact.extension}", artifact.version)
                }
            return Resolution(
                artifacts = artifacts,
                dependencyEdges = dependencyEdges(result.root),
                unresolvedCoordinates = emptyList(),
            )
        }
        if (!bestEffortEnabled) {
            val result = resolve(dependencies)
            return result.copy(artifacts = result.artifacts.distinctBy(ResolvedArtifact::path).sortedBy { it.path.toString() })
        }
        val resolved = dependencies.map { dependency ->
            dependency to runCatching { resolve(listOf(dependency)) }
        }
        return Resolution(
            artifacts = resolved.flatMap { (_, result) -> result.getOrNull()?.artifacts.orEmpty() }.distinctBy(ResolvedArtifact::path).sortedBy { it.path.toString() },
            dependencyEdges = resolved.flatMap { (_, result) -> result.getOrNull()?.dependencyEdges.orEmpty() }.distinct().sorted(),
            unresolvedCoordinates = resolved.mapNotNull { (dependency, result) -> result.exceptionOrNull()?.let { dependency.artifact.toString() } }.sorted(),
        )
    }

    private fun dependencyEdges(root: DependencyNode?): List<String> {
        val edges = mutableListOf<String>()
        fun coordinate(node: DependencyNode): String? = node.dependency?.artifact?.let { artifact ->
            val classifier = artifact.classifier.takeIf(String::isNotBlank)?.let { ":$it" }.orEmpty()
            "${artifact.groupId}:${artifact.artifactId}$classifier:${artifact.extension}:${artifact.version}"
        }
        fun visit(parent: DependencyNode, parentCoordinate: String?) {
            parent.children.orEmpty().forEach { child ->
                val target = coordinate(child)
                if (parentCoordinate != null && target != null) {
                    edges += "$parentCoordinate>${child.dependency?.scope ?: "compile"}>$target"
                }
                visit(child, target)
            }
        }
        root?.let { visit(it, coordinate(it)) }
        return edges.distinct().sorted()
    }
}
