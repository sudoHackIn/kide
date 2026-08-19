package dev.kide.worker

import java.nio.file.Path
import org.apache.maven.repository.internal.MavenRepositorySystemUtils
import org.eclipse.aether.DefaultRepositorySystemSession
import org.eclipse.aether.RepositorySystem
import org.eclipse.aether.artifact.DefaultArtifact
import org.eclipse.aether.collection.CollectRequest
import org.eclipse.aether.connector.basic.BasicRepositoryConnectorFactory
import org.eclipse.aether.graph.Dependency
import org.eclipse.aether.impl.DefaultServiceLocator
import org.eclipse.aether.repository.LocalRepository
import org.eclipse.aether.repository.RemoteRepository
import org.eclipse.aether.resolution.DependencyRequest
import org.eclipse.aether.spi.connector.RepositoryConnectorFactory
import org.eclipse.aether.spi.connector.transport.TransporterFactory
import org.eclipse.aether.transport.http.HttpTransporterFactory

/** Official Maven Resolver adapter for external artifacts only. */
internal object MavenExternalResolver {
    data class Resolution(val paths: List<Path>, val unresolvedCoordinates: List<String>)
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
        fun resolve(dependencies: List<Dependency>): List<Path> {
            val request = CollectRequest().apply {
                repositories = listOf(RemoteRepository.Builder("central", "default", "https://repo.maven.apache.org/maven2/").build())
                dependencies.forEach(::addDependency)
            }
            return repositorySystem.resolveDependencies(session, DependencyRequest(request, null)).artifactResults
                .mapNotNull { result -> result.artifact?.file?.toPath() }
                .filter { path -> path.fileName.toString().endsWith(".jar") }
        }
        if (!bestEffortEnabled) return Resolution(resolve(dependencies).distinct().sortedBy(Path::toString), emptyList())
        val resolved = dependencies.map { dependency ->
            dependency to runCatching { resolve(listOf(dependency)) }
        }
        return Resolution(
            paths = resolved.flatMap { (_, result) -> result.getOrDefault(emptyList()) }.distinct().sortedBy(Path::toString),
            unresolvedCoordinates = resolved.mapNotNull { (dependency, result) -> result.exceptionOrNull()?.let { dependency.artifact.toString() } }.sorted(),
        )
    }
}
