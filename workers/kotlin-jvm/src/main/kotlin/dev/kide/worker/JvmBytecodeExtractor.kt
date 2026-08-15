package dev.kide.worker

import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import java.util.jar.JarFile
import kotlin.io.path.isDirectory
import kotlin.io.path.isRegularFile
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import org.objectweb.asm.AnnotationVisitor
import org.objectweb.asm.ClassReader
import org.objectweb.asm.ClassVisitor
import org.objectweb.asm.FieldVisitor
import org.objectweb.asm.MethodVisitor
import org.objectweb.asm.Opcodes
import org.objectweb.asm.Type

/**
 * Extracts declarations from JVM binaries without loading their classes. One
 * resolved artifact becomes one virtual dependency source unit, keeping Core's
 * persistence transaction artifact-granular rather than class-granular.
 */
internal object JvmBytecodeExtractor {
    fun descriptor(artifact: Path, component: String, context: String): JsonElement {
        require(artifact.isRegularFile() || artifact.isDirectory()) { "artifact does not exist: $artifact" }
        val artifactHash = fingerprint(artifactBytes(artifact))
        return buildJsonObject {
            put("source_unit", buildJsonObject {
                put("id", "jvm:$artifactHash"); put("component", component)
                put("path", ".kide/dependencies/${artifactHash.removePrefix("sha256:")}")
                put("language", "java"); put("origin", "dependency"); put("content", artifactHash); put("context", context)
            })
            put("provenance", provenance(context))
            put("symbol_locators", buildJsonArray {
                classEntries(artifact).map { it.first.removeSuffix(".class").replace('/', '.') }
                    .filter { it.isNotEmpty() && !it.endsWith("module-info") && !it.endsWith("package-info") }
                    .distinct().sorted().forEach { name ->
                        add(buildJsonObject { put("qualified_name", name); put("symbol_id", "jvm:$artifactHash:$name") })
                    }
            })
        }
    }
    /**
     * Resolves a bounded set of K2 target keys to the durable IDs emitted by
     * [extract]. Only classes requested by K2 are opened; an overloaded JVM
     * member is intentionally omitted until K2 supplies its JVM descriptor.
     */
    fun resolvedTargetIds(classpath: List<Path>, targetKeys: Set<String>): Map<String, String> {
        val wanted = targetKeys.mapNotNull(::wantedTarget).groupBy { it.entry }
        if (wanted.isEmpty()) return emptyMap()
        val candidates = mutableMapOf<String, MutableSet<String>>()
        classpath.filter { it.isRegularFile() || it.isDirectory() }.distinct().sortedBy(Path::toString).forEach { artifact ->
            classEntriesFor(artifact, wanted.keys).forEach { (entry, bytes) ->
                val artifactHash = fingerprint(artifactBytes(artifact))
                val snapshot = snapshot(entry, bytes, artifactHash, component = "k2-external", context = "k2-external").jsonObject
                val symbols = snapshot["symbols"]!!.jsonArray.map { it.jsonObject }
                wanted.getValue(entry).forEach { target ->
                    val matches = symbols.filter { symbol -> target.matches(symbol) }.map { it.requiredString("id") }.distinct()
                    if (matches.size == 1) candidates.getOrPut(target.key, ::mutableSetOf).add(matches.single())
                }
            }
        }
        return candidates.mapNotNull { (key, ids) -> ids.singleOrNull()?.let { key to it } }.toMap()
    }

    fun extract(artifact: Path, component: String, context: String): List<JsonElement> {
        return listOf(extractArtifact(artifact, component, context))
    }

    fun extractArtifact(artifact: Path, component: String, context: String): JsonElement {
        require(artifact.isRegularFile() || artifact.isDirectory()) { "artifact does not exist: $artifact" }
        val artifactHash = fingerprint(artifactBytes(artifact))
        val unitId = "jvm:$artifactHash"
        val facts = classEntries(artifact).flatMap { (_, bytes) ->
            val language = if (hasKotlinMetadata(bytes)) "kotlin" else "java"
            val classFacts = ClassFacts(unitId, artifactHash, component, language)
            ClassReader(bytes).accept(classFacts, ClassReader.SKIP_CODE or ClassReader.SKIP_DEBUG or ClassReader.SKIP_FRAMES)
            listOf(classFacts)
        }
        return buildJsonObject {
            put("source_unit", buildJsonObject {
                put("id", unitId)
                put("component", component)
                put("path", ".kide/dependencies/${artifactHash.removePrefix("sha256:")}")
                put("language", "java")
                put("origin", "dependency")
                put("content", artifactHash)
                put("context", context)
            })
            put("symbols", buildJsonArray { facts.flatMap { it.symbols }.distinctBy { it.toString() }.sortedBy { it.toString() }.forEach(::add) })
            put("occurrences", buildJsonArray {})
            put("references", buildJsonArray {})
            put("calls", buildJsonArray {})
            put("hierarchy", buildJsonArray { facts.flatMap { it.hierarchy }.distinctBy { it.toString() }.sortedBy { it.toString() }.forEach(::add) })
            put("types", buildJsonArray {})
            put("diagnostics", buildJsonArray {})
            put("completeness", "partial")
            put("provenance", provenance(context))
        }
    }

    private fun classEntries(artifact: Path): List<Pair<String, ByteArray>> = when {
        artifact.isRegularFile() -> JarFile(artifact.toFile()).use { jar ->
            jar.entries().asSequence()
                .filter { entry ->
                    !entry.isDirectory && entry.name.endsWith(".class") &&
                        !entry.name.endsWith("module-info.class") &&
                        !entry.name.startsWith("META-INF/versions/")
                }
                .sortedBy { it.name }
                .map { entry -> entry.name to jar.getInputStream(entry).use { it.readBytes() } }
                .toList()
        }
        else -> Files.walk(artifact).use { paths ->
            paths.filter { it.isRegularFile() && it.fileName.toString().endsWith(".class") }
                .sorted()
                .map { path -> artifact.relativize(path).toString().replace('\\', '/') to Files.readAllBytes(path) }
                .toList()
        }
    }

    private fun classEntriesFor(artifact: Path, entries: Set<String>): List<Pair<String, ByteArray>> = when {
        artifact.isRegularFile() -> JarFile(artifact.toFile()).use { jar ->
            entries.sorted().mapNotNull { entry ->
                jar.getJarEntry(entry)?.let { found -> entry to jar.getInputStream(found).use { it.readBytes() } }
            }
        }
        else -> entries.sorted().mapNotNull { entry ->
            val file = artifact.resolve(entry)
            file.takeIf { it.isRegularFile() }?.let { entry to Files.readAllBytes(it) }
        }
    }

    private fun artifactBytes(artifact: Path): List<ByteArray> = when {
        artifact.isRegularFile() -> listOf(Files.readAllBytes(artifact))
        else -> Files.walk(artifact).use { paths ->
            paths.filter { it.isRegularFile() }.sorted().map { path ->
                artifact.relativize(path).toString().encodeToByteArray() + byteArrayOf(0) + Files.readAllBytes(path)
            }.toList()
        }
    }

    private fun snapshot(
        entry: String,
        bytes: ByteArray,
        artifactHash: String,
        component: String,
        context: String,
    ): JsonElement {
        val internalName = ClassReader(bytes).className
        val unitId = "jvm:$artifactHash:$entry"
        val path = ".kide/dependencies/${artifactHash.removePrefix("sha256:")}/$entry"
        val language = if (hasKotlinMetadata(bytes)) "kotlin" else "java"
        val facts = ClassFacts(unitId, artifactHash, component, language)
        ClassReader(bytes).accept(facts, ClassReader.SKIP_CODE or ClassReader.SKIP_DEBUG or ClassReader.SKIP_FRAMES)
        return buildJsonObject {
            put("source_unit", buildJsonObject {
                put("id", unitId)
                put("component", component)
                put("path", path)
                put("language", language)
                put("origin", "dependency")
                put("content", fingerprint(listOf(bytes)))
                put("context", context)
            })
            put("symbols", buildJsonArray { facts.symbols.forEach { add(it) } })
            put("occurrences", buildJsonArray {})
            put("references", buildJsonArray {})
            put("calls", buildJsonArray {})
            put("hierarchy", buildJsonArray { facts.hierarchy.forEach { add(it) } })
            put("types", buildJsonArray {})
            put("diagnostics", buildJsonArray {})
            put("completeness", "partial")
            put("provenance", provenance(context))
        }
    }

    private fun hasKotlinMetadata(bytes: ByteArray): Boolean {
        var present = false
        ClassReader(bytes).accept(object : ClassVisitor(Opcodes.ASM9) {
            override fun visitAnnotation(descriptor: String, visible: Boolean): AnnotationVisitor? {
                present = present || descriptor == "Lkotlin/Metadata;"
                return null
            }
        }, ClassReader.SKIP_CODE or ClassReader.SKIP_DEBUG or ClassReader.SKIP_FRAMES)
        return present
    }

    private class ClassFacts(
        private val unitId: String,
        private val artifactHash: String,
        private val component: String,
        private val language: String,
    ) : ClassVisitor(Opcodes.ASM9) {
        val symbols = mutableListOf<JsonElement>()
        val hierarchy = mutableListOf<JsonElement>()
        private lateinit var classId: String
        private lateinit var className: String
        private var classAccess: Int = 0
        private var classSignature: String? = null
        private val annotationTargets = mutableListOf<String>()

        override fun visit(version: Int, access: Int, name: String, signature: String?, superName: String?, interfaces: Array<out String>) {
            classId = symbolId(name)
            className = name.replace('/', '.')
            classAccess = access
            classSignature = signature
            (listOfNotNull(superName) + interfaces).filter { it != "java/lang/Object" }.sorted().forEach { parent ->
                hierarchy += buildJsonObject {
                    put("subtype", classId)
                    put("supertype", symbolId(parent))
                    put("precision", "exact")
                    put("provenance", provenance(artifactHash))
                }
            }
        }

        override fun visitAnnotation(descriptor: String, visible: Boolean): AnnotationVisitor? {
            annotationTargets += symbolId(Type.getType(descriptor).internalName)
            return null
        }

        override fun visitField(access: Int, name: String, descriptor: String, signature: String?, value: Any?): FieldVisitor? {
            symbols += symbol(
                id = "$classId#field:$name:$descriptor",
                kind = "field",
                name = name,
                qualifiedName = "$className.$name",
                signature = signature ?: Type.getType(descriptor).className,
                owner = classId,
                access = access,
            )
            return null
        }

        override fun visitMethod(access: Int, name: String, descriptor: String, signature: String?, exceptions: Array<out String>?): MethodVisitor? {
            val constructor = name == "<init>"
            symbols += symbol(
                id = "$classId#${if (constructor) "constructor" else "method"}:$name$descriptor",
                kind = if (constructor) "constructor" else "method",
                name = if (constructor) "<init>" else name,
                qualifiedName = "$className.${if (constructor) "<init>" else name}",
                signature = signature ?: descriptor,
                owner = classId,
                access = access,
            )
            return null
        }

        override fun visitEnd() {
            symbols += symbol(
                id = classId,
                kind = classKind(classAccess),
                name = className.substringAfterLast('.'),
                qualifiedName = className,
                signature = classSignature,
                owner = null,
                access = classAccess,
                annotationTargets = annotationTargets.sorted(),
            )
            symbols.sortBy { it.toString() }
        }

        private fun symbol(
            id: String, kind: String, name: String, qualifiedName: String, signature: String?, owner: String?, access: Int, annotationTargets: List<String> = emptyList(),
        ): JsonElement = buildJsonObject {
            put("id", id)
            put("backend_key", buildJsonObject { put("backend", WORKER_NAME); put("schema_version", 1); put("value", id) })
            put("language", language)
            put("kind", kind)
            put("name", name)
            put("qualified_name", qualifiedName)
            put("signature", signature)
            put("component", component)
            put("declaration", range())
            put("name_range", range())
            put("owner", owner)
            put("modifiers", buildJsonArray { modifiers(access).forEach { add(JsonPrimitive(it)) } })
            put("applied_symbols", buildJsonArray { annotationTargets.forEach { add(JsonPrimitive(it)) } })
            put("freshness", "fresh")
            put("completeness", "partial")
            put("provenance", provenance(artifactHash))
        }

        private fun range(): JsonElement = buildJsonObject {
            put("source_unit", unitId)
            put("bytes", buildJsonObject { put("start", 0); put("end", 0) })
        }

        private fun symbolId(internalName: String) = "jvm:$artifactHash:${internalName.replace('/', '.')}"
    }

    private fun classKind(access: Int) = when {
        access and Opcodes.ACC_ANNOTATION != 0 -> "interface"
        access and Opcodes.ACC_INTERFACE != 0 -> "interface"
        access and Opcodes.ACC_ENUM != 0 -> "enum"
        else -> "class"
    }

    private fun modifiers(access: Int): List<String> = buildList {
        if (access and Opcodes.ACC_PUBLIC != 0) add("public")
        if (access and Opcodes.ACC_PROTECTED != 0) add("protected")
        if (access and Opcodes.ACC_PRIVATE != 0) add("private")
        if (access and Opcodes.ACC_STATIC != 0) add("static")
        if (access and Opcodes.ACC_FINAL != 0) add("final")
        if (access and Opcodes.ACC_ABSTRACT != 0) add("abstract")
    }

    private fun provenance(analysisOptions: String): JsonElement = buildJsonObject {
        put("backend", WORKER_NAME)
        put("backend_version", WORKER_VERSION)
        put("protocol_version", WORKER_PROTOCOL_VERSION)
        put("analysis_options", analysisOptions)
    }

    private fun fingerprint(parts: List<ByteArray>): String {
        val digest = MessageDigest.getInstance("SHA-256")
        parts.forEach { part -> digest.update(part.size.toLong().toString().encodeToByteArray()); digest.update(0); digest.update(part) }
        return "sha256:${digest.digest().joinToString("") { "%02x".format(it) }}"
    }

    private fun wantedTarget(key: String): WantedTarget? = when {
        key.startsWith("class:") -> {
            val qualifiedName = key.removePrefix("class:")
            WantedTarget(key, "${qualifiedName.replace('.', '/')}.class", qualifiedName, null, false)
        }
        key.startsWith("callable:") -> {
            val callable = key.removePrefix("callable:").substringBefore('#')
            val split = callable.lastIndexOf('.')
            if (split <= 0) null else {
                val owner = callable.substring(0, split).replace('/', '.')
                val member = callable.substring(split + 1)
                WantedTarget(key, "${owner.replace('.', '/')}.class", owner, member, member == owner.substringAfterLast('.'))
            }
        }
        else -> null
    }

    private data class WantedTarget(
        val key: String,
        val entry: String,
        val owner: String,
        val member: String?,
        val constructor: Boolean,
    ) {
        fun matches(symbol: kotlinx.serialization.json.JsonObject): Boolean {
            val qualifiedName = symbol["qualified_name"]?.jsonPrimitive?.content ?: return false
            return when {
                member == null -> qualifiedName == owner && symbol.requiredString("kind") in setOf("class", "interface", "enum", "object")
                constructor -> symbol.requiredString("kind") == "constructor" && qualifiedName == "$owner.<init>"
                else -> symbol.requiredString("kind") in setOf("method", "field") && qualifiedName == "$owner.$member"
            }
        }
    }
}
