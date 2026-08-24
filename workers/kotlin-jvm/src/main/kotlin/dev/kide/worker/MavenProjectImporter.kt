package dev.kide.worker

import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import java.util.concurrent.TimeUnit
import java.io.StringReader
import java.io.StringWriter
import javax.xml.parsers.DocumentBuilderFactory
import javax.xml.transform.TransformerFactory
import javax.xml.transform.dom.DOMSource
import javax.xml.transform.stream.StreamResult
import kotlin.io.path.exists
import kotlin.io.path.isDirectory
import kotlin.io.path.isRegularFile
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import org.apache.maven.model.Model
import org.apache.maven.model.Plugin
import org.apache.maven.model.io.xpp3.MavenXpp3Reader
import org.codehaus.plexus.util.xml.Xpp3Dom

/**
 * Worker-local Maven reactor discovery for the supported v1 subset.
 *
 * Maven owns effective-model construction. The worker invokes a selected
 * local Maven only to obtain an effective POM, then emits the generic
 * manifest/source-set layer. Dependency artifact resolution is added
 * separately.
 */
internal object MavenProjectImporter {
    private const val MODEL_VERSION = "3.9.9"

    data class ResolvedArtifact(
        val path: Path,
        val component: String,
        val context: String,
        val coordinate: String,
        val version: String,
    )

    fun import(workspace: Path): JsonElement = import(workspace, System.getenv())

    internal fun import(workspace: Path, environment: Map<String, String>): JsonElement {
        require(workspace.isDirectory()) { "workspace root is not a directory: $workspace" }
        val root = workspace.toRealPath()
        val rootPom = root.resolve("pom.xml")
        require(rootPom.isRegularFile()) { "Maven workspace has no pom.xml at $rootPom" }
        val maven = selectMaven(root, environment)
        val modules = effectiveReactor(root, canonicalPath(root, rootPom, "Maven workspace POM"), maven, environment)
        // The manifest and every later worker lane must derive a component's
        // context from the same per-module resolution.  A workspace-wide
        // de-duplication of shared JAR paths loses the association with later
        // modules and lets dependency descriptors disagree with this manifest.
        val reactorCoordinates = modules.map { module -> modelKey(module.model) }.toSet()
        val resolutions = modules.associateWith { module ->
            MavenExternalResolver.resolveWithDiagnostics(module.model, reactorCoordinates)
        }
        val classpath = modules.associate { module ->
            componentId(root, module) to resolutions.getValue(module)
                .artifacts
                .map { artifact -> fingerprint(listOf(Files.readAllBytes(artifact.path))) }
                .distinct()
                .sorted()
        }
        val components = modules.map { module ->
            component(root, module, classpath[componentId(root, module)].orEmpty(), resolutions.getValue(module).dependencyEdges)
        }
        val configuration = fingerprint(modules.map { Files.readAllBytes(it.pom) })
        return buildJsonObject {
            put("workspace", "maven:${fingerprint(listOf(root.toString().encodeToByteArray()))}")
            put("root", ".")
            put("components", buildJsonArray { components.forEach(::add) })
            put("dependencies", buildJsonArray { })
            put("fingerprint", configuration)
            put("provenance", provenance(configuration))
        }
    }

    /** Resolve Maven's test classpath per reactor module; only regular JARs enter the catalog. */
    fun resolvedArtifacts(workspace: Path): List<ResolvedArtifact> = resolvedArtifacts(workspace, System.getenv())

    fun javaCompilationContexts(workspace: Path): List<JavaCompilationContext> {
        val root = workspace.toRealPath()
        val maven = selectMaven(root, System.getenv())
        val modules = effectiveReactor(root, canonicalPath(root, root.resolve("pom.xml"), "Maven workspace POM"), maven, System.getenv())
        val reactorCoordinates = modules.map { module -> modelKey(module.model) }.toSet()
        val sourcesByModule = modules.associateWith { module -> javaSources(root, module) }
        val sourceRootsByModule = modules.associateWith { module -> javaSourceRoots(root, module) }
        val modulesByCoordinate = modules.associateBy { module -> modelKey(module.model) }
        fun reactorClosure(module: Module): List<Module> {
            val visited = linkedSetOf<Module>()
            fun visit(candidate: Module) {
                if (!visited.add(candidate)) return
                candidate.model.dependencies.orEmpty()
                    .mapNotNull { dependency -> modulesByCoordinate["${dependency.groupId}:${dependency.artifactId}"] }
                    .forEach(::visit)
            }
            visit(module)
            return visited.toList()
        }
        return modules.map { module ->
            val ownedSources = sourcesByModule.getValue(module)
            val compilationSources = reactorClosure(module)
                .flatMap { dependency -> sourcesByModule.getValue(dependency) }
                .distinct().sortedBy(Path::toString)
            val compilationRoots = reactorClosure(module)
                .flatMap { dependency -> sourceRootsByModule.getValue(dependency) }
                .distinct().sortedBy(Path::toString)
            val resolution = MavenExternalResolver.resolveWithDiagnostics(module.model, reactorCoordinates)
            val classpathFingerprints = resolution.artifacts
                .map { artifact -> fingerprint(listOf(Files.readAllBytes(artifact.path))) }
                .distinct()
                .sorted()
            JavaCompilationContext(
                component = componentId(root, module),
                sourceFiles = compilationSources,
                ownedSourceFiles = ownedSources,
                sourceRoots = compilationRoots,
                classpath = resolution.paths,
                jdkHome = Path.of(System.getProperty("java.home")),
                languageLevel = javaLanguageLevel(module.model),
                unresolvedDependencies = resolution.unresolvedCoordinates,
                artifactContext = component(root, module, classpathFingerprints, resolution.dependencyEdges)
                    .jsonObject["configuration"]!!.jsonPrimitive.content,
            )
        }
    }

    private fun javaSources(root: Path, module: Module): List<Path> = sourceSets(root, module).flatMap { sourceSet -> sourceSet.roots }
        .flatMap { relative -> Files.walk(root.resolve(relative)).use { paths -> paths.filter { it.isRegularFile() && it.fileName.toString().endsWith(".java") }.toList() } }
        .distinct().sortedBy(Path::toString)

    private fun javaSourceRoots(root: Path, module: Module): List<Path> = sourceSets(root, module)
        .flatMap { sourceSet -> sourceSet.roots }
        .map { relative -> root.resolve(relative) }
        .filter { path -> Files.isDirectory(path) && Files.walk(path).use { paths -> paths.anyMatch { it.isRegularFile() && it.fileName.toString().endsWith(".java") } } }
        .distinct().sortedBy(Path::toString)

    internal fun resolvedArtifacts(workspace: Path, environment: Map<String, String>): List<ResolvedArtifact> {
        val root = workspace.toRealPath()
        val maven = selectMaven(root, environment)
        val modules = effectiveReactor(root, canonicalPath(root, root.resolve("pom.xml"), "Maven workspace POM"), maven, environment)
        val reactorCoordinates = modules.map { module -> modelKey(module.model) }.toSet()
        val resolutions = modules.associateWith { module ->
            MavenExternalResolver.resolveWithDiagnostics(module.model, reactorCoordinates)
        }
        val classpath = resolutions.mapValues { (_, resolution) ->
            resolution.artifacts.map { artifact -> fingerprint(listOf(Files.readAllBytes(artifact.path))) }
                .distinct()
                .sorted()
        }
        val contexts = modules.associate { module ->
            componentId(root, module) to component(root, module, classpath[module].orEmpty(), resolutions.getValue(module).dependencyEdges)
                .jsonObject["configuration"]!!.jsonPrimitive.content
        }
        return modules.flatMap { module ->
            if (module.model.packaging == "pom") return@flatMap emptyList()
            val component = componentId(root, module)
            resolutions.getValue(module).artifacts.map { artifact ->
                ResolvedArtifact(
                    artifact.path,
                    component,
                    contexts.getValue(component),
                    artifact.coordinate,
                    artifact.version,
                )
            }
        }.distinctBy { it.path.toAbsolutePath().normalize() }.sortedBy { it.path.toString() }
    }

    private fun reactor(root: Path, pom: Path): List<Module> {
        val seen = linkedSetOf<Path>()
        fun visit(candidate: Path): List<Module> {
            val canonicalPom = candidate.toRealPath()
            if (!seen.add(canonicalPom)) return emptyList()
            val model = read(canonicalPom)
            val module = Module(canonicalPom.parent, canonicalPom, model)
            val children = model.modules.orEmpty().sorted().flatMap { child ->
                val childPom = module.directory.resolve(child).normalize().resolve("pom.xml")
                require(childPom.isRegularFile()) {
                    "Maven reactor module `$child` declared by ${workspacePath(root, canonicalPom)} has no pom.xml"
                }
                visit(canonicalPath(root, childPom, "Maven reactor module `$child`"))
            }
            return listOf(module) + children
        }
        return visit(pom).sortedBy { workspacePath(root, it.directory) }
    }

    private fun effectiveReactor(root: Path, rootPom: Path, maven: Path, environment: Map<String, String>): List<Module> {
        val raw = reactor(root, rootPom)
        val effective = effectiveModels(maven, rootPom, environment).toMutableMap()
        // Some Maven/plugin combinations report only the requested project.
        // Fill only missing reactor entries; the normal path is one root call.
        raw.forEach { module ->
            val key = modelKey(module.model)
            if (key !in effective) effective.putAll(effectiveModels(maven, module.pom, environment))
        }
        return raw.map { module ->
            module.copy(model = requireNotNull(effective[modelKey(module.model)]) {
                "Maven effective model omitted ${modelKey(module.model)}"
            })
        }
    }

    private fun component(root: Path, module: Module, classpath: List<String>, resolvedEdges: List<String> = emptyList()): JsonElement {
        val sourceSets = sourceSets(root, module)
        val dependencyEdges = module.model.dependencies.orEmpty()
            .filter { dependency -> dependency.scope != "import" && dependency.optional != "true" }
            .map { dependency ->
                val classifier = dependency.classifier?.takeIf(String::isNotBlank)?.let { ":$it" }.orEmpty()
                "${dependency.scope ?: "compile"}:${dependency.groupId}:${dependency.artifactId}$classifier:${dependency.type ?: "jar"}:${dependency.version ?: "managed"}"
            }
        val configuration = ComponentContextDigest.fingerprint(
            component = componentId(root, module),
            sourceSets = sourceSets.map { sourceSet ->
                "${sourceSet.name}:${sourceSet.test}:${sourceSet.roots.joinToString(",")}:${sourceSet.languages.joinToString(",")}"
            },
            artifacts = classpath,
            dependencyEdges = dependencyEdges + resolvedEdges,
            toolchain = listOf(
                "build-tool=maven-model-$MODEL_VERSION",
                "jvm=${System.getProperty("java.version")}",
                "language-level=${javaLanguageLevel(module.model) ?: "default"}",
            ),
        )
        val relative = workspacePath(root, module.directory)
        val artifact = moduleArtifactId(root, module)
        return buildJsonObject {
            put("id", componentId(root, module))
            put("name", artifact)
            put("build_system", "maven")
            put("root", relative)
            put("languages", buildJsonArray {
                sourceSets.flatMap { it.languages }.distinct().sorted().forEach { add(JsonPrimitive(it)) }
            })
            put("configuration", configuration)
            put("source_sets", buildJsonArray { sourceSets.forEach { add(it.json) } })
            put("classpath", buildJsonArray { classpath.forEach { add(JsonPrimitive(it)) } })
            put("toolchain", buildJsonObject {
                put("jvm_version", System.getProperty("java.version"))
                put("build_tool_version", "maven-model-$MODEL_VERSION")
                put("kotlin_version", JsonPrimitive(null as String?))
            })
            put("compiler_configuration", configuration)
        }
    }

    /** Maven's effective model is the authority for the javac language level. */
    private fun javaLanguageLevel(model: Model): String? = listOf(
        model.properties?.getProperty("maven.compiler.release"),
        model.properties?.getProperty("maven.compiler.source"),
        model.properties?.getProperty("java.version"),
    ).firstOrNull { value -> value?.matches(Regex("\\d+")) == true }

    private fun sourceSets(root: Path, module: Module): List<SourceSet> = listOf(
        sourceSet(root, module, "main", test = false),
        sourceSet(root, module, "test", test = true),
    ).filter { it.roots.isNotEmpty() }

    private fun sourceSet(root: Path, module: Module, name: String, test: Boolean): SourceSet {
        val build = module.model.build
        val configured = if (test) build?.testSourceDirectory else build?.sourceDirectory
        val conventional = buildList {
            add("src/$name/java")
            if (hasKotlinPlugin(module.model)) add("src/$name/kotlin")
        }
        val roots = (listOfNotNull(configured) + conventional + kotlinSourceDirectories(module.model, test))
            .map { path -> module.directory.resolve(path).normalize() }
            .filter(Path::isDirectory)
            .map { path -> canonicalPath(root, path, "Maven source root") }
            .distinct()
            .sortedBy(Path::toString)
        return SourceSet(
            name,
            test,
            roots.map { path -> workspacePath(root, path) },
            roots.flatMap(::languages).distinct().sorted(),
        )
    }

    private fun languages(root: Path): List<String> = Files.walk(root).use { paths ->
        paths.filter(Path::isRegularFile).map { path ->
            when (path.fileName.toString().substringAfterLast('.', "")) {
                "java" -> "java"
                "kt", "kts" -> "kotlin"
                else -> null
            }
        }.filter { it != null }.map { it!! }.distinct().sorted().toList()
    }

    private fun read(pom: Path): Model = Files.newBufferedReader(pom).use { reader ->
        MavenXpp3Reader().read(reader)
    }

    private fun readEffectivePoms(pom: Path): List<Model> {
        val document = DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(pom.toFile())
        if (document.documentElement.tagName == "project") return listOf(read(pom))
        require(document.documentElement.tagName == "projects") { "unexpected Maven effective POM root" }
        val projects = document.documentElement.getElementsByTagName("project")
        return (0 until projects.length).map { index ->
            val xml = StringWriter().also { writer ->
                TransformerFactory.newInstance().newTransformer().transform(DOMSource(projects.item(index)), StreamResult(writer))
            }.toString()
            MavenXpp3Reader().read(StringReader(xml))
        }
    }

    private fun hasKotlinPlugin(model: Model): Boolean = model.build?.plugins.orEmpty().any { plugin ->
        plugin.isKideKotlinMavenPlugin()
    }

    private fun kotlinSourceDirectories(model: Model, test: Boolean): List<String> = model.build?.plugins.orEmpty()
        .filter { plugin -> plugin.isKideKotlinMavenPlugin() }
        .flatMap { plugin ->
            buildList {
                if (!test) addAll(sourceDirectories(plugin.configuration))
                plugin.executions.orEmpty()
                    .filter { execution -> execution.goals.orEmpty().any { goal -> goal == if (test) "test-compile" else "compile" } }
                    .flatMapTo(this) { execution -> sourceDirectories(execution.configuration) }
            }
        }.distinct().sorted()

    private fun Plugin.isKideKotlinMavenPlugin(): Boolean =
        artifactId == "kotlin-maven-plugin" && (groupId == null || groupId == "org.jetbrains.kotlin")

    private fun sourceDirectories(configuration: Any?): List<String> {
        val sourceDirs = (configuration as? Xpp3Dom)?.getChild("sourceDirs") ?: return emptyList()
        return sourceDirs.getChildren("source").mapNotNull(Xpp3Dom::getValue).filter(String::isNotBlank)
    }

    /**
     * Resolve Maven in the same order an IDE-managed worker would: an explicit
     * bundled distribution, the checked-in wrapper, then the user's PATH.
     * The command is deliberately kept worker-local and never enters Core
     * provenance or canonical records.
     */
    internal fun selectMaven(root: Path, environment: Map<String, String>): Path {
        environment["KIDE_MAVEN_HOME"]?.takeIf(String::isNotBlank)?.let { configured ->
            val executable = Path.of(configured).resolve("bin/mvn")
            require(executable.isRegularFile() && Files.isExecutable(executable)) {
                "KIDE_MAVEN_HOME does not contain an executable bin/mvn"
            }
            return executable
        }
        root.resolve("mvnw").takeIf { it.isRegularFile() && Files.isExecutable(it) }?.let { return it }
        environment["PATH"]?.split(java.io.File.pathSeparator)?.asSequence()
            ?.map(Path::of)?.map { it.resolve("mvn") }
            ?.firstOrNull { it.isRegularFile() && Files.isExecutable(it) }
            ?.let { return it }
        error("Maven is unavailable; set KIDE_MAVEN_HOME, add an executable mvnw, or put mvn on PATH")
    }

    private fun effectiveModels(maven: Path, pom: Path, environment: Map<String, String>): Map<String, Model> {
        val output = Files.createTempFile("kide-maven-effective-", ".xml")
        try {
            runMaven(maven, pom, listOf("-N", "help:effective-pom", "-Doutput=${output}"), environment)
            require(output.isRegularFile()) { "Maven effective-model import did not produce ${output.fileName}" }
            return readEffectivePoms(output).associateBy(::modelKey)
        } finally {
            Files.deleteIfExists(output)
        }
    }

    private fun runMaven(maven: Path, pom: Path, goals: List<String>, environment: Map<String, String>) {
        val process = ProcessBuilder(listOf(maven.toString(), "-B", "-q", "-f", pom.toString()) + goals)
            .redirectErrorStream(true).also { builder ->
                // Selection variables affect only launcher choice; replacing PATH
                // would also hide shell utilities used by wrapper scripts.
                builder.environment().putAll(environment.filterKeys { it !in setOf("PATH", "KIDE_MAVEN_HOME") })
            }.start()
        val log = process.inputStream.bufferedReader().use { it.readText() }
        require(process.waitFor(60, TimeUnit.SECONDS)) { "Maven import timed out" }
        require(process.exitValue() == 0) { "Maven import failed for ${pom.fileName}: ${log.takeLast(1_000)}" }
    }

    private fun componentId(root: Path, module: Module): String {
        // Keep IDs aligned with core discovery, which owns canonical component
        // identity for every build system. Maven coordinates are useful model
        // metadata, but are not part of a source-unit identity.
        val relative = workspacePath(root, module.directory)
        val rootKey = if (relative == ".") "root" else relative
        return "maven:$rootKey:main"
    }

    private fun modelKey(model: Model): String = "${model.groupId ?: model.parent?.groupId ?: "local"}:${requireNotNull(model.artifactId)}"

    private fun moduleArtifactId(root: Path, module: Module): String = requireNotNull(module.model.artifactId) {
        "Maven POM ${workspacePath(root, module.pom)} has no artifactId"
    }

    private fun canonicalPath(root: Path, path: Path, description: String): Path {
        val canonical = path.toRealPath()
        require(canonical.startsWith(root)) { "$description escapes workspace root" }
        return canonical
    }

    private fun workspacePath(root: Path, path: Path): String = root.relativize(path)
        .toString().replace('\\', '/').ifBlank { "." }

    private fun fingerprint(inputs: List<ByteArray>): String {
        val digest = MessageDigest.getInstance("SHA-256")
        inputs.forEach { input -> digest.update(input); digest.update(0) }
        return "sha256:" + digest.digest().joinToString("") { byte -> "%02x".format(byte) }
    }

    private fun provenance(configuration: String): JsonElement = buildJsonObject {
        put("backend", "kide-maven-model")
        put("backend_version", MODEL_VERSION)
        put("protocol_version", WORKER_PROTOCOL_VERSION)
        put("analysis_options", configuration)
    }

    private data class Module(val directory: Path, val pom: Path, val model: Model)

    private data class SourceSet(
        val name: String,
        val test: Boolean,
        val roots: List<String>,
        val languages: List<String>,
    ) {
        val json: JsonElement get() = buildJsonObject {
            put("name", name)
            put("source_roots", buildJsonArray { roots.forEach { add(JsonPrimitive(it)) } })
            put("generated_roots", buildJsonArray { })
            put("test", test)
        }
    }
}
