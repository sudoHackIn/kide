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
import kotlinx.serialization.json.JsonArray
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
        val contexts = JavaCompilationContexts.forWorkspace(workspaceRoot)
        require(selected.isNotEmpty()) { "Java analysis requires source files" }
        val sourceIdentities = sourceIdentities(selected, contexts, workspaceRoot)
        return JavaCompilationPlanner.shards(selected, contexts).flatMap { shard ->
            analyzePartition(shard.selected, shard.context, workspaceRoot, sourceIdentities)
        }
    }

    private fun sourceIdentities(
        selected: Map<Path, JsonElement>,
        contexts: List<JavaCompilationContext>,
        workspaceRoot: Path,
    ): Map<Path, JsonObject> {
        val fallbackContext = selected.values.first().jsonObject.requiredString("context")
        return buildMap {
            selected.forEach { (path, sourceUnit) -> put(path, sourceUnit.jsonObject) }
            contexts.forEach { compilation -> compilation.ownedSourceFiles.forEach { sourceFile ->
                val path = canonical(sourceFile)
                if (path !in this) {
                    val relativePath = workspaceRoot.relativize(path).toString()
                    put(path, buildJsonObject {
                        put("id", "java:${compilation.component}:$relativePath")
                        put("path", relativePath)
                        put("language", "java")
                        put("component", compilation.component)
                        put("context", fallbackContext)
                    })
                }
            } }
        }
    }

    private fun analyzePartition(
        selected: Map<Path, JsonElement>,
        context: JavaCompilationContext,
        workspaceRoot: Path,
        sourceIdentities: Map<Path, JsonObject>,
    ): List<JsonElement> {
        // Exact Java references require declaration ASTs for same-component
        // types. The context is already bounded to this component and its
        // project dependencies; a future two-phase declaration index can make
        // this file-sharded without weakening reference ownership.
        val allSources = (context.sourceFiles + selected.keys)
            .map(::canonical).distinct().sortedBy(Path::toString)
        val compiler = checkNotNull(ToolProvider.getSystemJavaCompiler()) { "a JDK with javac is required for Java indexing" }
        val diagnostics = DiagnosticCollector<JavaFileObject>()
        compiler.getStandardFileManager(diagnostics, null, Charsets.UTF_8).use { fileManager ->
            val units = fileManager.getJavaFileObjectsFromPaths(allSources)
            val options = buildList {
                add("-proc:none")
                context.languageLevel?.let { release ->
                    add("--release")
                    add(release)
                }
                context.sourceRoots.takeIf { roots -> roots.isNotEmpty() }?.let { roots ->
                    add("-sourcepath")
                    add(roots.joinToString(System.getProperty("path.separator")))
                }
                val classpath = context.classpath.distinct().filter(Files::exists)
                if (classpath.isNotEmpty()) {
                    add("-classpath")
                    add(classpath.joinToString(System.getProperty("path.separator")))
                }
            }
            val task = compiler.getTask(null, fileManager, diagnostics, options, null, units) as JavacTask
            val parsed = task.parse().toList()
            val analysisFailure = runCatching { task.analyze() }.exceptionOrNull()
            if (analysisFailure != null && !MavenExternalResolver.bestEffortEnabled) throw analysisFailure
            val trees = Trees.instance(task)
            val collector = FactCollector(trees, selected, sourceIdentities, allSources)
            if (analysisFailure == null) {
                collector.collectDeclarations(parsed)
                collector.collectReferences(parsed)
            }
            val snapshots = collector.snapshots(diagnostics.diagnostics).let { snapshots ->
                analysisFailure?.let { failure ->
                    snapshots.map { snapshot -> markPartial(withAnalysisFailureDiagnostic(snapshot.jsonObject, failure)) }
                } ?: snapshots
            }
            val externalKeys = snapshots.flatMap { snapshot -> snapshot.jsonObject["symbols"]!!.jsonArray.flatMap { symbol ->
                symbol.jsonObject["applied_symbols"]!!.jsonArray.mapNotNull { value -> value.jsonPrimitive.content.removePrefix("jvm:type:").takeIf { value.jsonPrimitive.content.startsWith("jvm:type:") }?.let { "class:$it" } }
            } }.toSortedSet()
            val resolved = JvmBytecodeExtractor.resolvedTargetIds(context.classpath.distinct(), externalKeys)
            val unresolvedDependencies = context.unresolvedDependencies.distinct().sorted()
            return snapshots.map { snapshot -> remapExternalAnnotations(snapshot.jsonObject, resolved) }
                .let { snapshots ->
                    if (unresolvedDependencies.isEmpty()) snapshots
                    else snapshots.map { snapshot -> markPartial(withUnresolvedDependencyDiagnostics(snapshot, unresolvedDependencies)) }
                }
        }
    }

    private fun withUnresolvedDependencyDiagnostics(snapshot: JsonObject, unresolved: List<String>) = buildJsonObject {
        snapshot.forEach { (key, value) ->
            if (key != "diagnostics") put(key, value) else put(key, buildJsonArray {
                value.jsonArray.forEach(::add)
                val source = snapshot.getValue("source_unit").jsonObject.requiredString("id")
                val provenance = snapshot.getValue("provenance")
                unresolved.forEach { coordinate -> add(buildJsonObject {
                    put("source_unit", source)
                    put("message", "Maven dependency is unavailable and was excluded from the analysis classpath: $coordinate")
                    put("severity", "warning")
                    put("freshness", "fresh")
                    put("completeness", "partial")
                    put("provenance", provenance)
                }) }
            })
        }
    }

    private fun withAnalysisFailureDiagnostic(snapshot: JsonObject, failure: Throwable) = buildJsonObject {
        snapshot.forEach { (key, value) ->
            if (key != "diagnostics") put(key, value) else put(key, buildJsonArray {
                value.jsonArray.forEach(::add)
                val source = snapshot.getValue("source_unit").jsonObject.requiredString("id")
                val provenance = snapshot.getValue("provenance")
                add(buildJsonObject {
                    put("source_unit", source)
                    put("message", "Java semantic analysis failed for this Maven module and was skipped in best-effort mode: ${failure::class.simpleName}")
                    put("severity", "warning")
                    put("freshness", "fresh")
                    put("completeness", "partial")
                    put("provenance", provenance)
                })
            })
        }
    }

    private fun markPartial(value: JsonElement): JsonElement = when (value) {
        is JsonObject -> buildJsonObject {
            value.forEach { (key, nested) ->
                put(key, if (key == "completeness") JsonPrimitive("partial") else markPartial(nested))
            }
        }
        is JsonArray -> buildJsonArray { value.forEach { add(markPartial(it)) } }
        else -> value
    }

    private fun remapExternalAnnotations(snapshot: JsonObject, resolved: Map<String, String>) = buildJsonObject {
        snapshot.forEach { (key, value) ->
            if (key != "symbols") put(key, value) else put("symbols", buildJsonArray {
                value.jsonArray.forEach { symbol -> add(buildJsonObject {
                    symbol.jsonObject.forEach { (symbolKey, symbolValue) ->
                        if (symbolKey != "applied_symbols") put(symbolKey, symbolValue) else put("applied_symbols", buildJsonArray {
                            symbolValue.jsonArray.forEach { annotation ->
                                val raw = annotation.jsonPrimitive.content
                                add(JsonPrimitive(resolved["class:${raw.removePrefix("jvm:type:")}"] ?: raw))
                            }
                        })
                    }
                }) }
            })
        }
    }

    private class FactCollector(
        private val trees: Trees,
        private val selected: Map<Path, JsonElement>,
        private val sourceIdentities: Map<Path, JsonObject>,
        allSources: List<Path>,
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
            val id = elementId(element, path, start, end) ?: return
            elementIds[element] = id
            val owner = element.enclosingElement?.let(::elementId)
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
            val targetId = elementId(target) ?: return
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

        /**
         * SOURCE_PATH lets javac load a reactor type without making it an
         * explicit batch input. Derive the same stable ID lazily so references
         * from the shard still point at that type's eventual snapshot.
         */
        private fun elementId(element: Element): String? = elementIds[element] ?: trees.getPath(element)?.let { path ->
            val sourcePath = sourcePath(path.compilationUnit) ?: return@let null
            val positions = trees.sourcePositions
            val start = positions.getStartPosition(path.compilationUnit, path.leaf).toInt()
            val end = positions.getEndPosition(path.compilationUnit, path.leaf).toInt()
            elementId(element, sourcePath, start, end)
        }

        private fun elementId(element: Element, path: Path, start: Int, end: Int): String? {
            if (start < 0 || end < start) return null
            val source = sourceIdentity(path) ?: return null
            val text = contents[path] ?: return null
            val name = element.simpleName.toString()
            val nameStart = nameOffset(text, name, start, end)
            return "java:${source.requiredString("component")}:${source.requiredString("path")}#${kind(element)}:$name:$nameStart"
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
            put("source_unit", source.requiredString("id")); put("message", diagnostic.getMessage(null)); put("severity", diagnosticSeverity(diagnostic.kind)); put("range", buildJsonObject { put("start", utf8(text, start)); put("end", utf8(text, end)) }); put("freshness", "fresh"); put("completeness", "complete"); put("provenance", provenance)
        }

        private fun diagnosticSeverity(kind: javax.tools.Diagnostic.Kind): String = when (kind) {
            javax.tools.Diagnostic.Kind.ERROR -> "error"
            javax.tools.Diagnostic.Kind.WARNING, javax.tools.Diagnostic.Kind.MANDATORY_WARNING -> "warning"
            javax.tools.Diagnostic.Kind.NOTE -> "information"
            javax.tools.Diagnostic.Kind.OTHER -> "hint"
        }

        private fun sourcePath(unit: CompilationUnitTree): Path? = runCatching { canonical(Path.of(unit.sourceFile.toUri())) }.getOrNull()
        private fun sourceIdentity(path: Path): JsonObject? = sourceIdentities[path]
        private fun range(unit: String, text: String, start: Int, end: Int) = buildJsonObject { put("source_unit", unit); put("bytes", buildJsonObject { put("start", utf8(text, start)); put("end", utf8(text, end)) }) }
        private fun utf8(text: String, offset: Int) = text.substring(0, offset.coerceIn(0, text.length)).encodeToByteArray().size
        private fun nameOffset(text: String, name: String, start: Int, end: Int): Int = Regex("\\b${Regex.escape(name)}\\b").find(text, start)?.range?.first?.takeIf { it < end } ?: start
        private fun kind(element: Element): String = when (element.kind) { ElementKind.CLASS -> "class"; ElementKind.INTERFACE -> "interface"; ElementKind.ENUM -> "enum"; ElementKind.ANNOTATION_TYPE -> "interface"; ElementKind.CONSTRUCTOR -> "constructor"; ElementKind.METHOD -> "method"; ElementKind.FIELD, ElementKind.ENUM_CONSTANT -> "field"; else -> "property" }
        private fun appliedSymbols(element: Element): List<String> = element.annotationMirrors
            .map { annotation -> elementIds[annotation.annotationType.asElement()] ?: "jvm:type:${qualifiedName(annotation.annotationType.asElement())}" }
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
