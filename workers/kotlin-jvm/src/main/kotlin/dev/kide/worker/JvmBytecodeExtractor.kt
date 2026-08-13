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
import kotlinx.serialization.json.put
import org.objectweb.asm.AnnotationVisitor
import org.objectweb.asm.ClassReader
import org.objectweb.asm.ClassVisitor
import org.objectweb.asm.FieldVisitor
import org.objectweb.asm.MethodVisitor
import org.objectweb.asm.Opcodes
import org.objectweb.asm.Type

/**
 * Extracts declarations from JVM binaries without loading their classes. Each
 * class becomes a virtual dependency source unit, so Core can persist it using
 * the same atomic snapshot path as source-backed facts.
 */
internal object JvmBytecodeExtractor {
    fun extract(artifact: Path, component: String, context: String): List<JsonElement> {
        require(artifact.isRegularFile() || artifact.isDirectory()) { "artifact does not exist: $artifact" }
        val artifactHash = fingerprint(artifactBytes(artifact))
        return classEntries(artifact).map { (entry, bytes) ->
            snapshot(entry, bytes, artifactHash, component, context)
        }
    }

    private fun classEntries(artifact: Path): List<Pair<String, ByteArray>> = when {
        artifact.isRegularFile() -> JarFile(artifact.toFile()).use { jar ->
            jar.entries().asSequence()
                .filter { !it.isDirectory && it.name.endsWith(".class") && !it.name.endsWith("module-info.class") }
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
        private val annotations = mutableListOf<String>()

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
            annotations += Type.getType(descriptor).className
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
                annotations = emptyList(),
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
                annotations = emptyList(),
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
                annotations = annotations.sorted(),
            )
            symbols.sortBy { it.toString() }
        }

        private fun symbol(
            id: String, kind: String, name: String, qualifiedName: String, signature: String?, owner: String?, access: Int, annotations: List<String>,
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
            put("annotations", buildJsonArray { annotations.forEach { add(JsonPrimitive(it)) } })
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
}
