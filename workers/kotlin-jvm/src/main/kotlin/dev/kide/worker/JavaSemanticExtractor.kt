package dev.kide.worker

import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import javax.lang.model.element.Element
import javax.lang.model.element.ElementKind
import javax.lang.model.element.TypeElement
import javax.tools.DiagnosticCollector
import javax.tools.JavaFileObject
import javax.tools.StandardJavaFileManager
import javax.tools.ToolProvider
import com.sun.source.tree.ClassTree
import com.sun.source.tree.CompilationUnitTree
import com.sun.source.tree.IdentifierTree
import com.sun.source.tree.MemberSelectTree
import com.sun.source.tree.MethodInvocationTree
import com.sun.source.tree.MethodTree
import com.sun.source.tree.Tree
import com.sun.source.tree.VariableTree
import com.sun.source.util.JavacTask
import com.sun.source.util.TreePath
import com.sun.source.util.TreePathScanner
import com.sun.source.util.Trees
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

/**
 * Source-semantic Java adapter. It confines javac's mutable compiler graph to
 * one request and emits only canonical JSON facts before returning.
 */
internal object JavaSemanticExtractor {
    fun analyze(sourceUnits: List<JsonElement>, workspaceRoot: Path): List<JsonElement> {
        val selected = sourceUnits.associateBy { canonical(workspaceRoot.resolve(it.jsonObject.requiredString("path"))) }
        val contexts = GradleProjectImporter.javaCompilationContexts(workspaceRoot).values
        val allSources = (contexts.flatMap { it.sourceFiles } + selected.keys).map(::canonical).distinct().sortedBy(Path::toString)
        require(allSources.isNotEmpty()) { "Java analysis requires source files" }
        val compiler = checkNotNull(ToolProvider.getSystemJavaCompiler()) { "a JDK with javac is required for Java indexing" }
        val diagnostics = DiagnosticCollector<JavaFileObject>()
        compiler.getStandardFileManager(diagnostics, null, Charsets.UTF_8).use { fileManager ->
            val units = fileManager.getJavaFileObjectsFromPaths(allSources)
            val options = buildList {
                add("-proc:none")
                val classpath = contexts.flatMap { it.classpath }.distinct().filter(Files::exists)
                if (classpath.isNotEmpty()) {
                    add("-classpath")
                    add(classpath.joinToString(System.getProperty("path.separator")))
                }
            }
            val task = compiler.getTask(null, fileManager, diagnostics, options, null, units) as JavacTask
            val parsed = task.parse().toList()
            task.analyze()
            val trees = Trees.instance(task)
            val collector = FactCollector(trees, selected, allSources, workspaceRoot)
            collector.collectDeclarations(parsed)
            collector.collectReferences(parsed)
            return collector.snapshots(diagnostics.diagnostics)
        }
    }

    private class FactCollector(
        private val trees: Trees,
        private val selected: Map<Path, JsonElement>,
        allSources: List<Path>,
        private val workspaceRoot: Path,
    ) : TreePathScanner<Unit, Unit>() {
        private val symbols = linkedMapOf<Path, MutableList<Symbol>>()
        private val occurrences = linkedMapOf<Path, MutableList<Occurrence>>()
        private val hierarchy = linkedMapOf<Path, MutableList<Pair<String, String>>>()
        private val elementIds = mutableMapOf<Element, String>()
        private val contents = allSources.associateWith(Files::readString)
        private var collectingReferences = false

        fun collectDeclarations(units: List<CompilationUnitTree>) {
            collectingReferences = false
            units.forEach { scan(TreePath(it), null) }
        }

        fun collectReferences(units: List<CompilationUnitTree>) {
            collectingReferences = true
            units.forEach { scan(TreePath(it), null) }
        }

        override fun visitClass(node: ClassTree, unused: Unit?) {
            if (!collectingReferences) declaration(node, trees.getElement(getCurrentPath()))
            super.visitClass(node, unused)
        }

        override fun visitMethod(node: MethodTree, unused: Unit?) {
            if (!collectingReferences) declaration(node, trees.getElement(getCurrentPath()))
            super.visitMethod(node, unused)
        }

        override fun visitVariable(node: VariableTree, unused: Unit?) {
            val element = trees.getElement(getCurrentPath())
            if (!collectingReferences && (element?.kind == ElementKind.FIELD || element?.kind == ElementKind.ENUM_CONSTANT)) declaration(node, element)
            super.visitVariable(node, unused)
        }

        override fun visitMethodInvocation(node: MethodInvocationTree, unused: Unit?) {
            if (collectingReferences) reference(TreePath(getCurrentPath(), node.methodSelect), isCall = true)
            super.visitMethodInvocation(node, unused)
        }

        override fun visitIdentifier(node: IdentifierTree, unused: Unit?) {
            if (collectingReferences) reference(getCurrentPath(), isCall = false)
            super.visitIdentifier(node, unused)
        }

        override fun visitMemberSelect(node: MemberSelectTree, unused: Unit?) {
            if (collectingReferences) reference(getCurrentPath(), isCall = false)
            super.visitMemberSelect(node, unused)
        }

        private fun declaration(tree: Tree, element: Element?) {
            if (element == null) return
            val path = sourcePath(getCurrentPath().compilationUnit) ?: return
            val source = sourceIdentity(path) ?: return
            val text = contents.getValue(path)
            val positions = trees.sourcePositions
            val start = positions.getStartPosition(getCurrentPath().compilationUnit, tree).toInt()
            val end = positions.getEndPosition(getCurrentPath().compilationUnit, tree).toInt()
            if (start < 0 || end < start) return
            val name = element.simpleName.toString()
            val nameStart = nameOffset(text, name, start, end)
            val id = "java:${source.requiredString("component")}:${source.requiredString("path")}#${kind(element)}:$name:$nameStart"
            elementIds[element] = id
            val owner = element.enclosingElement?.let(elementIds::get)
            if (path in selected) symbols.getOrPut(path, ::mutableListOf) += Symbol(id, element, start, end, nameStart, nameStart + name.length, owner)
            if (path in selected && element is TypeElement) {
                element.superclass?.let { mirror -> (mirror as? javax.lang.model.type.DeclaredType)?.asElement()?.let(elementIds::get) }
                    ?.let { superId -> hierarchy.getOrPut(path, ::mutableListOf) += id to superId }
                element.interfaces.mapNotNull { mirror -> (mirror as? javax.lang.model.type.DeclaredType)?.asElement()?.let(elementIds::get) }
                    .forEach { superId -> hierarchy.getOrPut(path, ::mutableListOf) += id to superId }
            }
        }

        private fun reference(path: TreePath, isCall: Boolean) {
            val tree = path.leaf
            val sourcePath = sourcePath(path.compilationUnit) ?: return
            if (sourcePath !in selected) return
            val target = trees.getElement(path) ?: return
            val targetId = elementIds[target] ?: return
            val text = contents.getValue(sourcePath)
            val start = trees.sourcePositions.getStartPosition(path.compilationUnit, tree).toInt()
            val end = trees.sourcePositions.getEndPosition(path.compilationUnit, tree).toInt()
            if (start < 0 || end <= start) return
            val owner = symbols[sourcePath].orEmpty().filter { it.start <= start && end <= it.end && it.id != targetId }.minByOrNull { it.end - it.start }?.id
            occurrences.getOrPut(sourcePath, ::mutableListOf) += Occurrence(start, end, targetId, owner, isCall, trees.getTypeMirror(path)?.toString())
        }

        fun snapshots(diagnostics: List<javax.tools.Diagnostic<out JavaFileObject>>): List<JsonElement> = selected.entries.sortedBy { it.key.toString() }.map { (path, sourceUnit) ->
            val source = sourceUnit.jsonObject
            val text = contents.getValue(path)
            val provenance = provenance(source.requiredString("context"))
            val fileSymbols = symbols[path].orEmpty().sortedBy { it.start }
            val fileOccurrences = occurrences[path].orEmpty()
                .filter { occurrence -> fileSymbols.none { it.nameStart == occurrence.start && it.nameEnd == occurrence.end } }
                .distinctBy { listOf(it.start, it.end, it.target, it.isCall) }
            buildJsonObject {
                put("source_unit", sourceUnit)
                put("structural_fingerprint", fingerprint(text.encodeToByteArray()))
                put("public_api_fingerprint", fingerprint(fileSymbols.joinToString("|") { "${it.element.kind}:${it.element}" }.encodeToByteArray()))
                put("symbols", buildJsonArray { fileSymbols.forEach { add(symbolJson(it, source, text, provenance)) } })
                put("occurrences", buildJsonArray { fileOccurrences.forEach { add(occurrenceJson(it, source, text, provenance)) } })
                put("references", buildJsonArray { fileOccurrences.filterNot { it.isCall }.forEach { add(edgeJson(it, source, text, provenance, false)) } })
                put("calls", buildJsonArray { fileOccurrences.filter { it.isCall }.forEach { add(edgeJson(it, source, text, provenance, true)) } })
                put("hierarchy", buildJsonArray { hierarchy[path].orEmpty().distinct().forEach { (subtype, supertype) -> add(buildJsonObject { put("subtype", subtype); put("supertype", supertype); put("precision", "exact"); put("provenance", provenance) }) } })
                put("types", buildJsonArray { fileOccurrences.mapNotNull { it.typeDisplay }.distinct().sorted().forEach { add(typeJson(it, provenance)) } })
                put("diagnostics", buildJsonArray { diagnostics.filter { diagnostic -> diagnostic.source?.toUri()?.let { Path.of(it).toAbsolutePath().normalize() } == path }.forEach { diagnostic -> add(diagnosticJson(diagnostic, source, text, provenance)) } })
                put("completeness", "complete")
                put("provenance", provenance)
            }
        }

        private fun symbolJson(symbol: Symbol, source: JsonObject, text: String, provenance: JsonElement) = buildJsonObject {
            put("id", symbol.id); put("backend_key", buildJsonObject { put("backend", WORKER_NAME); put("schema_version", 1); put("value", symbol.element.toString()) })
            put("language", "java"); put("kind", kind(symbol.element)); put("name", symbol.element.simpleName.toString()); put("qualified_name", qualifiedName(symbol.element)); put("signature", symbol.element.toString())
            put("component", source.requiredString("component")); put("declaration", range(source.requiredString("id"), text, symbol.start, symbol.end)); put("name_range", range(source.requiredString("id"), text, symbol.nameStart, symbol.nameEnd)); put("owner", symbol.owner)
            put("modifiers", buildJsonArray { symbol.element.modifiers.map { it.name.lowercase() }.sorted().forEach { add(JsonPrimitive(it)) } }); put("applied_symbols", buildJsonArray { appliedSymbols(symbol.element).forEach { add(JsonPrimitive(it)) } })
            put("freshness", "fresh"); put("completeness", "complete"); put("provenance", provenance)
        }

        private fun occurrenceJson(occurrence: Occurrence, source: JsonObject, text: String, provenance: JsonElement) = buildJsonObject {
            put("range", range(source.requiredString("id"), text, occurrence.start, occurrence.end)); put("kind", if (occurrence.isCall) "call" else "reference"); put("enclosing_symbol", occurrence.owner); put("target", occurrence.target); put("type_id", occurrence.typeDisplay?.let(::typeId)); put("precision", "exact"); put("freshness", "fresh"); put("completeness", "complete"); put("provenance", provenance)
        }

        private fun edgeJson(occurrence: Occurrence, source: JsonObject, text: String, provenance: JsonElement, call: Boolean) = buildJsonObject {
            put("source", occurrenceJson(occurrence, source, text, provenance)); put("target", occurrence.target); if (call) put("caller", occurrence.owner); put("precision", "exact")
        }

        private fun diagnosticJson(diagnostic: javax.tools.Diagnostic<out JavaFileObject>, source: JsonObject, text: String, provenance: JsonElement) = buildJsonObject {
            val start = diagnostic.startPosition.toInt().coerceAtLeast(0)
            val end = diagnostic.endPosition.toInt().coerceAtLeast(start)
            put("source_unit", source.requiredString("id")); put("message", diagnostic.getMessage(null)); put("severity", diagnostic.kind.name.lowercase()); put("range", buildJsonObject { put("start", utf8(text, start)); put("end", utf8(text, end)) }); put("freshness", "fresh"); put("completeness", "complete"); put("provenance", provenance)
        }

        private fun sourcePath(unit: CompilationUnitTree): Path? = runCatching { canonical(Path.of(unit.sourceFile.toUri())) }.getOrNull()
        private fun sourceIdentity(path: Path): JsonObject? {
            selected[path]?.jsonObject?.let { return it }
            val template = selected.values.firstOrNull()?.jsonObject ?: return null
            val templatePath = template.requiredString("path")
            val relativePath = workspaceRoot.relativize(path).toString()
            return buildJsonObject {
                put("id", template.requiredString("id").removeSuffix(templatePath) + relativePath)
                put("path", relativePath)
                put("language", "java")
                put("component", template.requiredString("component"))
                put("context", template.requiredString("context"))
            }
        }
        private fun range(unit: String, text: String, start: Int, end: Int) = buildJsonObject { put("source_unit", unit); put("bytes", buildJsonObject { put("start", utf8(text, start)); put("end", utf8(text, end)) }) }
        private fun utf8(text: String, offset: Int) = text.substring(0, offset.coerceIn(0, text.length)).encodeToByteArray().size
        private fun nameOffset(text: String, name: String, start: Int, end: Int): Int = Regex("\\b${Regex.escape(name)}\\b").find(text, start)?.range?.first?.takeIf { it < end } ?: start
        private fun kind(element: Element): String = when (element.kind) { ElementKind.CLASS -> "class"; ElementKind.INTERFACE -> "interface"; ElementKind.ENUM -> "enum"; ElementKind.ANNOTATION_TYPE -> "interface"; ElementKind.CONSTRUCTOR -> "constructor"; ElementKind.METHOD -> "method"; ElementKind.FIELD, ElementKind.ENUM_CONSTANT -> "field"; else -> "property" }
        private fun appliedSymbols(element: Element): List<String> = element.annotationMirrors
            .mapNotNull { annotation -> elementIds[annotation.annotationType.asElement()] }
            .distinct()
            .sorted()
        private fun qualifiedName(element: Element): String = when (element) { is TypeElement -> element.qualifiedName.toString(); else -> "${element.enclosingElement?.let(::qualifiedName).orEmpty()}.${element.simpleName}".trim('.') }
        private fun typeJson(display: String, provenance: JsonElement) = buildJsonObject { put("id", typeId(display)); put("language", "java"); put("display", display); put("backend_key", buildJsonObject { put("backend", WORKER_NAME); put("schema_version", 1); put("value", display) }); put("freshness", "fresh"); put("completeness", "complete"); put("provenance", provenance) }
        private fun typeId(display: String): String { val hash = MessageDigest.getInstance("SHA-256").digest(display.encodeToByteArray()).joinToString("") { "%02x".format(it) }; return "java:type:$hash" }
    }

    private data class Symbol(val id: String, val element: Element, val start: Int, val end: Int, val nameStart: Int, val nameEnd: Int, val owner: String?)
    private data class Occurrence(val start: Int, val end: Int, val target: String, val owner: String?, val isCall: Boolean, val typeDisplay: String?)

    private fun provenance(context: String): JsonElement = buildJsonObject {
        put("backend", WORKER_NAME); put("backend_version", WORKER_VERSION); put("protocol_version", WORKER_PROTOCOL_VERSION); put("analysis_options", context)
    }

    private fun fingerprint(bytes: ByteArray): String {
        val hash = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
        return "sha256:$hash"
    }

    private fun canonical(path: Path): Path = runCatching { path.toRealPath() }.getOrElse { path.toAbsolutePath().normalize() }
}
