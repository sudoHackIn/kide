package dev.kide.worker

import java.io.File
import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import java.util.Properties
import kotlin.io.path.isDirectory
import kotlin.io.path.isRegularFile
import org.gradle.tooling.GradleConnector
import org.gradle.tooling.model.build.BuildEnvironment
import org.gradle.tooling.model.idea.IdeaDependency
import org.gradle.tooling.model.idea.IdeaModule
import org.gradle.tooling.model.idea.IdeaModuleDependency
import org.gradle.tooling.model.idea.IdeaProject
import org.gradle.tooling.model.idea.IdeaSingleEntryLibraryDependency
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * Explicit Gradle Tooling API importer for the MVP Kotlin/JVM subset.
 *
 * It intentionally consumes IDE tooling models rather than parsing Gradle DSL.
 * Gradle evaluates the project using its wrapper/daemon; this worker then exits
 * after returning only normalized data to Core.
 */
internal object GradleProjectImporter {
    data class ResolvedArtifact(val path: Path, val component: String, val context: String) {
        val cursor: String get() = path.toAbsolutePath().normalize().toString()
    }

    /** Worker-local K2 inputs. Neither file locations nor JDK details enter the protocol. */
    data class KotlinCompilationContext(
        val component: String,
        val sourceFiles: List<Path>,
        val classpath: List<Path>,
        val jdkHome: Path,
    )

    fun import(workspace: Path): JsonElement {
        require(Files.isDirectory(workspace)) { "workspace root is not a directory: $workspace" }
        val canonicalRoot = workspace.toRealPath()
        connector(canonicalRoot)
            .connect()
            .use { connection ->
                val environment = connection.getModel(BuildEnvironment::class.java)
                val project = connection.getModel(IdeaProject::class.java)
                return manifest(canonicalRoot, environment, project)
            }
    }

    /** Local artifact locations are intentionally exposed only inside the worker. */
    fun resolvedArtifacts(workspace: Path): List<ResolvedArtifact> {
        val root = workspace.toRealPath()
        connector(root).connect().use { connection ->
            val environment = connection.getModel(BuildEnvironment::class.java)
            val project = connection.getModel(IdeaProject::class.java)
            return project.modules.flatMap { module ->
                val context = fingerprint(listOf(
                    "gradle=${environment.gradle.gradleVersion}".encodeToByteArray(),
                    "module=${module.gradleProject.path}".encodeToByteArray(),
                ))
                module.dependencies.filterIsInstance<IdeaSingleEntryLibraryDependency>().map { dependency ->
                    ResolvedArtifact(dependency.file.toPath(), componentId(module), context)
                }
            }.distinctBy { it.path.toAbsolutePath().normalize() }.sortedBy { it.path.toString() }
        }
    }

    fun kotlinCompilationContexts(workspace: Path): Map<String, KotlinCompilationContext> {
        val root = workspace.toRealPath()
        connector(root).connect().use { connection ->
            val environment = connection.getModel(BuildEnvironment::class.java)
            val project = connection.getModel(IdeaProject::class.java)
            return project.modules.associate { module ->
                val libraries = module.dependencies.filterIsInstance<IdeaSingleEntryLibraryDependency>()
                    .map { dependency -> dependency.file.toPath().toAbsolutePath().normalize() }
                    .filter(Files::exists)
                    .distinct()
                    .sortedBy(Path::toString)
                componentId(module) to KotlinCompilationContext(
                    component = componentId(module),
                    sourceFiles = kotlinSourceFiles(module),
                    classpath = libraries,
                    jdkHome = environment.java.javaHome.toPath().toAbsolutePath().normalize(),
                )
            }
        }
    }

    private fun kotlinSourceFiles(module: IdeaModule): List<Path> = module.contentRoots
        .flatMap { root -> root.sourceDirectories + root.testDirectories }
        .filterNot { directory -> directory.isGenerated }
        .flatMap { directory ->
            val root = directory.directory.toPath()
            if (!Files.isDirectory(root)) emptyList() else Files.walk(root).use { paths ->
                paths.filter { path -> path.isRegularFile() && path.fileName.toString().endsWith(".kt") }.toList()
            }
        }
        .map { path -> path.toAbsolutePath().normalize() }
        .distinct()
        .sortedBy(Path::toString)

    /**
     * Tooling API otherwise follows the wrapper URL even when the wrapper has
     * already installed that Gradle version locally. A cold worker must be
     * able to reuse that installation and remain usable offline.
     */
    private fun connector(root: Path): GradleConnector = GradleConnector.newConnector()
        .forProjectDirectory(root.toFile())
        .also { connector -> resolveGradleInstallation(root)?.let { connector.useInstallation(it.toFile()) } }

    /**
     * Resolves the local distribution without asking Tooling API to fetch the
     * wrapper URL. An explicit worker setting wins so a caller may use a
     * managed Gradle installation even when a project has a wrapper.
     */
    internal fun resolveGradleInstallation(
        root: Path,
        environment: Map<String, String> = System.getenv(),
    ): Path? {
        environment["KIDE_GRADLE_INSTALLATION"]
            ?.takeIf(String::isNotBlank)
            ?.let(Path::of)
            ?.takeIf(::isGradleInstallation)
            ?.let { return it }
        val gradleUserHome = environment["GRADLE_USER_HOME"]
            ?.takeIf(String::isNotBlank)
            ?.let(Path::of)
            ?: Path.of(System.getProperty("user.home"), ".gradle")
        return localWrapperInstallation(root, gradleUserHome)
    }

    private fun localWrapperInstallation(root: Path, gradleUserHome: Path): Path? {
        val wrapper = root.resolve("gradle/wrapper/gradle-wrapper.properties")
        if (!wrapper.isRegularFile()) return null
        val properties = Properties().also { Files.newInputStream(wrapper).use(it::load) }
        val distribution = properties.getProperty("distributionUrl")
            ?.substringAfterLast('/')
            ?.removeSuffix(".zip")
            ?: return null
        val candidates = gradleUserHome.resolve("wrapper/dists").resolve(distribution)
        if (!candidates.isDirectory()) return null
        return Files.walk(candidates, 3).use { paths ->
            paths.filter(::isGradleInstallation).findFirst().orElse(null)
        }
    }

    private fun isGradleInstallation(path: Path): Boolean = path.resolve("bin/gradle").isRegularFile()

    private fun manifest(
        root: Path,
        environment: BuildEnvironment,
        project: IdeaProject,
    ): JsonElement {
        val modules = project.modules.sortedBy { it.gradleProject.path }
        val componentIds = modules.associate { it.name to componentId(it) }
        val moduleFacts = modules.map { module -> moduleFacts(root, environment, module, componentIds) }
        val dependencies = moduleFacts.flatMap { it.dependencies }.sortedWith(
            compareBy<DependencyFact>({ it.from }, { it.scope }, { it.target }),
        )
        val configuration = fingerprint(
            buildList {
                addAll(configurationInputBytes(root))
                add("gradle=${environment.gradle.gradleVersion}".encodeToByteArray())
                add("jvm=${gradleJvmVersion(environment)}".encodeToByteArray())
                moduleFacts.forEach { add(it.compilerConfiguration.encodeToByteArray()) }
            },
        )

        return buildJsonObject {
            put("workspace", "gradle:${fingerprint(listOf(root.toString().encodeToByteArray()))}")
            put("root", ".")
            put("components", buildJsonArray {
                moduleFacts.forEach { add(it.component) }
            })
            put("dependencies", buildJsonArray {
                dependencies.forEach { dependency ->
                    add(buildJsonObject {
                        put("from", dependency.from)
                        put("scope", dependency.scope)
                        put("target", dependency.targetJson)
                    })
                }
            })
            put("fingerprint", configuration)
            put("provenance", provenance(configuration))
        }
    }

    private fun moduleFacts(
        root: Path,
        environment: BuildEnvironment,
        module: IdeaModule,
        componentIds: Map<String, String>,
    ): ModuleFacts {
        val sourceSets = sourceSets(root, module)
        val externalArtifacts = module.dependencies.filterIsInstance<IdeaSingleEntryLibraryDependency>()
            .map { dependency -> artifactFingerprint(dependency.file.toPath()) }
            .distinct()
            .sorted()
        val compilerConfiguration = fingerprint(
            buildList {
                add("gradle=${environment.gradle.gradleVersion}".encodeToByteArray())
                add("jvm=${gradleJvmVersion(environment)}".encodeToByteArray())
                add("module=${module.gradleProject.path}".encodeToByteArray())
                sourceSets.forEach { add(it.canonicalText.encodeToByteArray()) }
                externalArtifacts.forEach { add(it.encodeToByteArray()) }
            },
        )
        val dependencies = module.dependencies.mapNotNull { dependency ->
            dependencyFact(componentId(module), dependency, componentIds)
        }
        val languageNames = languages(sourceSets)
        val component = buildJsonObject {
            put("id", componentId(module))
            put("name", module.gradleProject.name)
            put("build_system", "gradle")
            put("root", workspacePath(root, module.gradleProject.projectDirectory.toPath()))
            put("languages", buildJsonArray { languageNames.forEach { add(JsonPrimitive(it)) } })
            put("configuration", compilerConfiguration)
            put("source_sets", buildJsonArray { sourceSets.forEach { add(it.json) } })
            put("classpath", buildJsonArray { externalArtifacts.forEach { add(JsonPrimitive(it)) } })
            put("toolchain", buildJsonObject {
                put("jvm_version", gradleJvmVersion(environment))
                put("gradle_version", environment.gradle.gradleVersion)
                // Tooling API exposes the Gradle JVM, not the Kotlin plugin version.
                put("kotlin_version", JsonPrimitive(KotlinVersion.CURRENT.toString()))
            })
            put("compiler_configuration", compilerConfiguration)
        }
        return ModuleFacts(component, dependencies, compilerConfiguration)
    }

    private fun sourceSets(root: Path, module: IdeaModule): List<SourceSetFact> {
        fun directories(test: Boolean): List<SourceDirectoryFact> = module.contentRoots
            .flatMap { contentRoot ->
                (if (test) contentRoot.testDirectories else contentRoot.sourceDirectories)
                    .map { directory ->
                        SourceDirectoryFact(
                            workspacePath(root, directory.directory.toPath()),
                            directory.isGenerated,
                        )
                    }
            }
            .distinct()
            .sortedBy { it.path }

        return buildList {
            val main = directories(test = false)
            if (main.isNotEmpty()) add(SourceSetFact("main", main, test = false))
            val test = directories(test = true)
            if (test.isNotEmpty()) add(SourceSetFact("test", test, test = true))
        }
    }

    private fun dependencyFact(
        from: String,
        dependency: IdeaDependency,
        componentIds: Map<String, String>,
    ): DependencyFact? = when (dependency) {
        is IdeaModuleDependency -> {
            val target = componentIds[dependency.targetModuleName] ?: return null
            DependencyFact(
                from = from,
                scope = dependency.scope.scope,
                target = "component:$target",
                targetJson = buildJsonObject {
                    put("target_kind", "component")
                    put("component", target)
                },
            )
        }
        is IdeaSingleEntryLibraryDependency -> {
            val artifact = artifactFingerprint(dependency.file.toPath())
            DependencyFact(
                from = from,
                scope = dependency.scope.scope,
                target = "artifact:$artifact",
                targetJson = buildJsonObject {
                    put("target_kind", "artifact")
                    put("content", artifact)
                },
            )
        }
        else -> null
    }

    private fun componentId(module: IdeaModule): String =
        "gradle:${module.gradleProject.path.ifBlank { ":" }}:main"

    private fun gradleJvmVersion(environment: BuildEnvironment): String =
        // Tooling API exposes the daemon JDK home and JVM arguments, but no
        // Java-version property. The importer itself runs with the configured
        // JVM in the supported MVP, and records its stable runtime version.
        System.getProperty("java.version") + "@" + environment.java.javaHome.name

    private fun languages(_sourceSets: List<SourceSetFact>): List<String> {
        // Source-set directory names are not language labels. The first MVP
        // supports both, and the later PSI phase assigns a unit language.
        return listOf("kotlin", "java")
    }

    private fun workspacePath(root: Path, path: Path): String {
        val canonical = path.toAbsolutePath().normalize()
        require(canonical.startsWith(root)) { "model path escapes workspace: $path" }
        val relative = root.relativize(canonical).toString().replace(File.separatorChar, '/')
        return relative.ifEmpty { "." }
    }

    private fun configurationInputBytes(root: Path): List<ByteArray> =
        Files.walk(root, 3).use { paths ->
            paths.filter { path ->
                path.isRegularFile() && path.fileName.toString() in configurationNames
            }.sorted().map { path ->
                val relative = workspacePath(root, path)
                relative.encodeToByteArray() + byteArrayOf(0) + Files.readAllBytes(path)
            }.toList()
        }

    private fun artifactFingerprint(path: Path): String =
        when {
            path.isRegularFile() -> fingerprint(listOf(Files.readAllBytes(path)))
            path.isDirectory() -> Files.walk(path).use { paths ->
                fingerprint(paths.filter { it.isRegularFile() }.sorted().map { file ->
                    path.relativize(file).toString().encodeToByteArray() + byteArrayOf(0) + Files.readAllBytes(file)
                }.toList())
            }
            else -> fingerprint(listOf(path.toString().encodeToByteArray()))
        }

    private fun fingerprint(parts: List<ByteArray>): String {
        val digest = MessageDigest.getInstance("SHA-256")
        parts.forEach { part ->
            digest.update(part.size.toLong().toString().encodeToByteArray())
            digest.update(0)
            digest.update(part)
        }
        return "sha256:${digest.digest().joinToString("") { byte -> "%02x".format(byte) }}"
    }

    private fun provenance(configuration: String): JsonElement = buildJsonObject {
        put("backend", WORKER_NAME)
        put("backend_version", WORKER_VERSION)
        put("protocol_version", WORKER_PROTOCOL_VERSION)
        put("analysis_options", configuration)
    }

    private data class ModuleFacts(
        val component: JsonElement,
        val dependencies: List<DependencyFact>,
        val compilerConfiguration: String,
    )

    private data class DependencyFact(
        val from: String,
        val scope: String,
        val target: String,
        val targetJson: JsonElement,
    )

    private data class SourceDirectoryFact(
        val path: String,
        val generated: Boolean,
    )

    private data class SourceSetFact(
        val name: String,
        val directories: List<SourceDirectoryFact>,
        val test: Boolean,
    ) {
        val sourceRoots = directories.filterNot { it.generated }.map { it.path }
        val generatedRoots = directories.filter { it.generated }.map { it.path }
        val canonicalText = "$name:$test:${sourceRoots.joinToString(",")}:${generatedRoots.joinToString(",")}"
        val json: JsonElement = buildJsonObject {
            put("name", name)
            put("source_roots", stringArray(sourceRoots))
            put("generated_roots", stringArray(generatedRoots))
            put("test", test)
        }
    }

    private fun stringArray(values: List<String>): JsonArray = buildJsonArray {
        values.forEach { add(JsonPrimitive(it)) }
    }

    private val configurationNames = setOf(
        "settings.gradle",
        "settings.gradle.kts",
        "build.gradle",
        "build.gradle.kts",
        "gradle.properties",
        "libs.versions.toml",
    )
}
