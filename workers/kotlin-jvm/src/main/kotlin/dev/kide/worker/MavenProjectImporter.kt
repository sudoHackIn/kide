package dev.kide.worker

import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import kotlin.io.path.exists
import kotlin.io.path.isDirectory
import kotlin.io.path.isRegularFile
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.apache.maven.model.Model
import org.apache.maven.model.io.xpp3.MavenXpp3Reader

/**
 * Worker-local Maven reactor discovery for the supported v1 subset.
 *
 * Maven's typed model reader owns XML parsing. This importer deliberately
 * emits only the generic manifest/source-set layer; effective dependency
 * resolution and language compilation contexts are added separately.
 */
internal object MavenProjectImporter {
    private const val MODEL_VERSION = "3.9.9"

    fun import(workspace: Path): JsonElement {
        require(workspace.isDirectory()) { "workspace root is not a directory: $workspace" }
        val root = workspace.toRealPath()
        val rootPom = root.resolve("pom.xml")
        require(rootPom.isRegularFile()) { "Maven workspace has no pom.xml at $rootPom" }
        val modules = reactor(root, rootPom)
        val components = modules.map { module -> component(root, module) }
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
                require(childPom.normalize().startsWith(root)) {
                    "Maven reactor module `$child` escapes workspace root"
                }
                visit(childPom)
            }
            return listOf(module) + children
        }
        return visit(pom).sortedBy { workspacePath(root, it.directory) }
    }

    private fun component(root: Path, module: Module): JsonElement {
        val sourceSets = sourceSets(root, module)
        val configuration = fingerprint(
            listOf(
                Files.readAllBytes(module.pom),
                sourceSets.joinToString("\n").encodeToByteArray(),
            ),
        )
        val relative = workspacePath(root, module.directory)
        val group = module.model.groupId ?: module.model.parent?.groupId ?: "local"
        val artifact = requireNotNull(module.model.artifactId) {
            "Maven POM ${workspacePath(root, module.pom)} has no artifactId"
        }
        return buildJsonObject {
            put("id", "maven:$group:$artifact:$relative:main")
            put("name", artifact)
            put("build_system", "maven")
            put("root", relative)
            put("languages", buildJsonArray {
                sourceSets.flatMap { it.languages }.distinct().sorted().forEach { add(JsonPrimitive(it)) }
            })
            put("configuration", configuration)
            put("source_sets", buildJsonArray { sourceSets.forEach { add(it.json) } })
            put("classpath", buildJsonArray { })
            put("toolchain", buildJsonObject {
                put("jvm_version", System.getProperty("java.version"))
                put("build_tool_version", "maven-model-$MODEL_VERSION")
                put("kotlin_version", JsonPrimitive(null as String?))
            })
            put("compiler_configuration", configuration)
        }
    }

    private fun sourceSets(root: Path, module: Module): List<SourceSet> = listOf(
        sourceSet(root, module, "main", test = false),
        sourceSet(root, module, "test", test = true),
    ).filter { it.roots.isNotEmpty() }

    private fun sourceSet(root: Path, module: Module, name: String, test: Boolean): SourceSet {
        val build = module.model.build
        val configured = if (test) build?.testSourceDirectory else build?.sourceDirectory
        val conventional = listOf("src/$name/java", "src/$name/kotlin")
        val roots = (listOfNotNull(configured) + conventional)
            .map { path -> module.directory.resolve(path).normalize() }
            .filter { path -> path.isDirectory() && path.startsWith(root) }
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
