package dev.kide.worker

import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import org.jetbrains.kotlin.K1Deprecation
import org.jetbrains.kotlin.cli.jvm.compiler.EnvironmentConfigFiles
import org.jetbrains.kotlin.cli.jvm.compiler.KotlinCoreEnvironment
import org.jetbrains.kotlin.com.intellij.openapi.util.Disposer
import org.jetbrains.kotlin.com.intellij.psi.PsiErrorElement
import org.jetbrains.kotlin.com.intellij.psi.PsiWhiteSpace
import org.jetbrains.kotlin.config.CompilerConfiguration
import org.jetbrains.kotlin.psi.KtClass
import org.jetbrains.kotlin.psi.KtClassBody
import org.jetbrains.kotlin.psi.KtConstructor
import org.jetbrains.kotlin.psi.KtFile
import org.jetbrains.kotlin.psi.KtImportDirective
import org.jetbrains.kotlin.psi.KtNamedDeclaration
import org.jetbrains.kotlin.psi.KtNamedFunction
import org.jetbrains.kotlin.psi.KtObjectDeclaration
import org.jetbrains.kotlin.psi.KtParameter
import org.jetbrains.kotlin.psi.KtProperty
import org.jetbrains.kotlin.psi.KtSecondaryConstructor
import org.jetbrains.kotlin.psi.KtTypeAlias
import org.jetbrains.kotlin.psi.KtTypeReference
import org.jetbrains.kotlin.psi.KtCallExpression
import org.jetbrains.kotlin.psi.KtAnnotationEntry
import org.jetbrains.kotlin.psi.KtNameReferenceExpression
import org.jetbrains.kotlin.psi.KtPsiFactory
import org.jetbrains.kotlin.psi.psiUtil.collectDescendantsOfType
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

/** PSI-only extraction. It produces exact syntax/ranges but never semantic targets. */
@OptIn(K1Deprecation::class, CompilerConfiguration.Internals::class)
internal class KotlinStructuralExtractor : AutoCloseable {
    private val disposable = Disposer.newDisposable("kide-kotlin-psi")
    private val environment = KotlinCoreEnvironment.createForProduction(
        disposable,
        CompilerConfiguration(),
        EnvironmentConfigFiles.JVM_CONFIG_FILES,
    )
    private val factory = KtPsiFactory(environment.project, false)

    fun analyze(sourceUnit: JsonElement, workspaceRoot: Path): JsonElement {
        val source = sourceUnit.jsonObject
        val path = source.requiredString("path")
        val contents = Files.readString(workspaceRoot.resolve(path))
        val file = factory.createFile(Path.of(path).fileName.toString(), contents)
        val unitId = source.requiredString("id")
        val context = source.requiredString("context")
        val provenance = provenance(context)
        val symbols = symbols(file, source, contents, provenance)
        val declarationRanges = symbols.map { it.nameStart to it.nameEnd }.toSet()
        val occurrences = occurrences(file, unitId, contents, declarationRanges, provenance)
        val diagnostics = diagnostics(file, unitId, contents, provenance)
        return buildJsonObject {
            put("source_unit", sourceUnit)
            put("structural_fingerprint", fingerprint(contents.encodeToByteArray()))
            put("public_api_fingerprint", publicApiFingerprint(symbols))
            put("symbols", buildJsonArray { symbols.forEach { add(it.json) } })
            put("occurrences", buildJsonArray { occurrences.forEach { add(it) } })
            put("references", buildJsonArray {})
            put("calls", buildJsonArray {})
            put("hierarchy", buildJsonArray {})
            put("types", buildJsonArray {})
            put("diagnostics", buildJsonArray { diagnostics.forEach { add(it) } })
            put("completeness", "partial")
            put("provenance", provenance)
        }
    }

    override fun close() {
        Disposer.dispose(disposable)
    }

    private fun symbols(
        file: KtFile,
        source: kotlinx.serialization.json.JsonObject,
        contents: String,
        provenance: JsonElement,
    ): List<SymbolFact> {
        val named = file.collectDescendantsOfType<KtNamedDeclaration>()
        .mapNotNull { declaration ->
            val name = declaration.name ?: return@mapNotNull null
            val nameIdentifier = declaration.nameIdentifier ?: return@mapNotNull null
            val declarationRange = declaration.textRange
            val nameRange = nameIdentifier.textRange
            val kind = symbolKind(declaration)
            val symbolId = "kotlin:${source.requiredString("component")}:${source.requiredString("path")}#$kind:$name:${nameRange.startOffset}"
            SymbolFact(
                nameStart = nameRange.startOffset,
                nameEnd = nameRange.endOffset,
                json = buildJsonObject {
                    put("id", symbolId)
                    put("backend_key", buildJsonObject {
                        put("backend", WORKER_NAME)
                        put("schema_version", 1)
                        put("value", "${source.requiredString("path")}:${nameRange.startOffset}:$kind")
                    })
                    put("language", "kotlin")
                    put("kind", kind)
                    put("name", name)
                    put("qualified_name", qualifiedName(file, declaration))
                    put("signature", structuralSignature(declaration))
                    put("component", source.requiredString("component"))
                    put("declaration", sourceRange(source.requiredString("id"), contents, declarationRange.startOffset, declarationRange.endOffset))
                    put("name_range", sourceRange(source.requiredString("id"), contents, nameRange.startOffset, nameRange.endOffset))
                    put("owner", ownerSymbolId(source, declaration))
                    put("modifiers", stringArray(modifiers(declaration)))
                    put("applied_symbols", stringArray(emptyList()))
                    put("freshness", "fresh")
                    put("completeness", "partial")
                    put("provenance", provenance)
                },
            )
        }
        val constructors = buildList {
            file.collectDescendantsOfType<KtClass>().mapNotNullTo(this) { clazz ->
                clazz.nameIdentifier?.let { primaryConstructorFact(file, clazz, source, contents, provenance) }
            }
            file.collectDescendantsOfType<KtSecondaryConstructor>().mapTo(this) { constructor ->
                constructorFact(file, constructor, source, contents, provenance)
            }
        }
        return (named + constructors)
            .sortedWith(compareBy<SymbolFact>({ it.nameStart }, { it.nameEnd }, { it.json.toString() }))
    }

    private fun constructorFact(
        file: KtFile,
        constructor: KtSecondaryConstructor,
        source: kotlinx.serialization.json.JsonObject,
        contents: String,
        provenance: JsonElement,
    ): SymbolFact {
        val range = constructor.textRange
        val keyword = constructor.getConstructorKeyword().textRange
        val owner = generateSequence(constructor.parent) { it.parent }
            .filterIsInstance<KtNamedDeclaration>()
            .firstOrNull()
        val ownerName = owner?.name ?: "anonymous"
        val ownerOffset = owner?.nameIdentifier?.textRange?.startOffset ?: range.startOffset
        val symbolId = "kotlin:${source.requiredString("component")}:${source.requiredString("path")}#constructor:<init>:${keyword.startOffset}"
        return SymbolFact(
            nameStart = keyword.startOffset,
            nameEnd = keyword.endOffset,
            json = buildJsonObject {
                put("id", symbolId)
                put("backend_key", buildJsonObject {
                    put("backend", WORKER_NAME)
                    put("schema_version", 1)
                    put("value", "${source.requiredString("path")}:${keyword.startOffset}:constructor")
                })
                put("language", "kotlin")
                put("kind", "constructor")
                put("name", "<init>")
                put("qualified_name", "${file.packageFqName.asString()}.$ownerName.<init>")
                put("signature", constructor.valueParameters.joinToString(",", prefix = "(", postfix = ")") { it.typeReference?.text ?: "?" })
                put("component", source.requiredString("component"))
                put("declaration", sourceRange(source.requiredString("id"), contents, range.startOffset, range.endOffset))
                put("name_range", sourceRange(source.requiredString("id"), contents, keyword.startOffset, keyword.endOffset))
                put("owner", "kotlin:${source.requiredString("component")}:${source.requiredString("path")}#class:$ownerName:$ownerOffset")
                put("modifiers", stringArray(modifiers(constructor)))
                put("applied_symbols", stringArray(emptyList()))
                put("freshness", "fresh")
                put("completeness", "partial")
                put("provenance", provenance)
            },
        )
    }

    private fun primaryConstructorFact(
        file: KtFile,
        owner: KtClass,
        source: kotlinx.serialization.json.JsonObject,
        contents: String,
        provenance: JsonElement,
    ): SymbolFact {
        val ownerName = owner.name ?: "anonymous"
        val constructor = owner.primaryConstructor
        val nameRange = owner.nameIdentifier?.textRange ?: owner.textRange
        val range = constructor?.textRange ?: nameRange
        val symbolId = "kotlin:${source.requiredString("component")}:${source.requiredString("path")}#constructor:<init>:${range.startOffset}"
        return SymbolFact(
            nameStart = nameRange.startOffset,
            nameEnd = nameRange.endOffset,
            json = buildJsonObject {
                put("id", symbolId)
                put("backend_key", buildJsonObject {
                    put("backend", WORKER_NAME)
                    put("schema_version", 1)
                    put("value", "${source.requiredString("path")}:${range.startOffset}:primary_constructor")
                })
                put("language", "kotlin")
                put("kind", "constructor")
                put("name", "<init>")
                put("qualified_name", "${file.packageFqName.asString()}.$ownerName.<init>")
                put("signature", constructor?.valueParameters?.joinToString(",", prefix = "(", postfix = ")") { it.typeReference?.text ?: "?" } ?: "()")
                put("component", source.requiredString("component"))
                put("declaration", sourceRange(source.requiredString("id"), contents, range.startOffset, range.endOffset))
                put("name_range", sourceRange(source.requiredString("id"), contents, nameRange.startOffset, nameRange.endOffset))
                put("owner", ownerSymbolId(source, owner))
                put("modifiers", stringArray(constructor?.let(::modifiers) ?: emptyList()))
                put("applied_symbols", stringArray(emptyList()))
                put("freshness", "fresh")
                put("completeness", "partial")
                put("provenance", provenance)
            },
        )
    }

    private fun occurrences(
        file: KtFile,
        unitId: String,
        contents: String,
        declarationRanges: Set<Pair<Int, Int>>,
        provenance: JsonElement,
    ): List<JsonElement> {
        val facts = mutableListOf<JsonElement>()
        facts += file.collectDescendantsOfType<KtImportDirective>().mapNotNull { directive ->
            directive.importedReference?.textRange?.let { range -> occurrence(unitId, contents, range.startOffset, range.endOffset, "import", provenance) }
        }
        facts += file.collectDescendantsOfType<KtTypeReference>().map { reference ->
            val range = reference.textRange
            occurrence(unitId, contents, range.startOffset, range.endOffset, "type_reference", provenance)
        }
        facts += file.collectDescendantsOfType<KtCallExpression>().mapNotNull { call ->
            call.calleeExpression?.textRange?.let { range -> occurrence(unitId, contents, range.startOffset, range.endOffset, "call", provenance) }
        }
        facts += file.collectDescendantsOfType<KtNameReferenceExpression>()
            .filter { reference ->
                val range = reference.textRange
                (range.startOffset to range.endOffset) !in declarationRanges
            }
            .map { reference ->
                val range = reference.textRange
                occurrence(unitId, contents, range.startOffset, range.endOffset, "reference", provenance)
            }
        return facts.distinctBy { it.toString() }.sortedBy { fact: JsonElement ->
            fact.jsonObject["range"]!!.jsonObject["bytes"]!!.jsonObject["start"]!!.jsonPrimitive.content.toLong()
        }
    }

    private fun diagnostics(file: KtFile, unitId: String, contents: String, provenance: JsonElement): List<JsonElement> =
        file.collectDescendantsOfType<PsiErrorElement>().map { error ->
            val range = error.textRange
            buildJsonObject {
                put("source_unit", unitId)
                put("range", byteRange(contents, range.startOffset, range.endOffset))
                put("severity", "error")
                put("code", "kotlin_parse_error")
                put("message", error.errorDescription)
                put("freshness", "fresh")
                put("completeness", "partial")
                put("provenance", provenance)
            }
        }

    private fun occurrence(
        unitId: String,
        contents: String,
        start: Int,
        end: Int,
        kind: String,
        provenance: JsonElement,
    ): JsonElement = buildJsonObject {
        put("range", sourceRange(unitId, contents, start, end))
        put("kind", kind)
        put("enclosing_symbol", null)
        put("target", null)
        put("type_id", null)
        put("precision", "approximate")
        put("freshness", "fresh")
        put("completeness", "partial")
        put("provenance", provenance)
    }

    private fun sourceRange(unitId: String, contents: String, start: Int, end: Int): JsonElement = buildJsonObject {
        put("source_unit", unitId)
        put("bytes", byteRange(contents, start, end))
    }

    private fun byteRange(contents: String, start: Int, end: Int): JsonElement = buildJsonObject {
        put("start", utf8Offset(contents, start))
        put("end", utf8Offset(contents, end))
    }

    /** Kotlin PSI uses UTF-16 offsets; KIDE persists UTF-8 byte offsets. */
    private fun utf8Offset(contents: String, utf16Offset: Int): Int {
        require(utf16Offset in 0..contents.length) { "PSI offset outside source text" }
        require(utf16Offset == 0 || utf16Offset == contents.length || !contents[utf16Offset - 1].isHighSurrogate() || !contents[utf16Offset].isLowSurrogate()) {
            "PSI offset splits a Unicode surrogate pair"
        }
        return contents.substring(0, utf16Offset).encodeToByteArray().size
    }

    private fun symbolKind(declaration: KtNamedDeclaration): String = when (declaration) {
        is KtClass -> when {
            declaration.isInterface() -> "interface"
            declaration.isEnum() -> "enum"
            else -> "class"
        }
        is KtObjectDeclaration -> "object"
        is KtNamedFunction -> if (declaration.parent is KtClassBody) "method" else "function"
        is KtConstructor<*> -> "constructor"
        is KtProperty -> if (declaration.parent is KtClassBody) "field" else "property"
        is KtParameter -> "parameter"
        is KtTypeAlias -> "type_alias"
        else -> "other"
    }

    private fun qualifiedName(file: KtFile, declaration: KtNamedDeclaration): String {
        val owners = generateSequence(declaration.parent) { it.parent }
            .filterIsInstance<KtNamedDeclaration>()
            .mapNotNull { it.name }
            .toList()
            .asReversed()
        return (listOf(file.packageFqName.asString()).filter { it.isNotEmpty() } + owners + listOfNotNull(declaration.name)).joinToString(".")
    }

    private fun structuralSignature(declaration: KtNamedDeclaration): String? = when (declaration) {
        is KtNamedFunction -> declaration.valueParameters.joinToString(",", prefix = "(", postfix = ")") { parameter -> parameter.typeReference?.text ?: "?" } + ":" + (declaration.typeReference?.text ?: "?")
        is KtProperty -> declaration.typeReference?.text
        is KtTypeAlias -> declaration.getTypeReference()?.text
        else -> null
    }

    private fun ownerSymbolId(source: kotlinx.serialization.json.JsonObject, declaration: KtNamedDeclaration): String? {
        val owner = generateSequence(declaration.parent) { it.parent }.filterIsInstance<KtNamedDeclaration>().firstOrNull() ?: return null
        val name = owner.name ?: return null
        return "kotlin:${source.requiredString("component")}:${source.requiredString("path")}#${symbolKind(owner)}:$name:${owner.nameIdentifier?.textRange?.startOffset ?: owner.textRange.startOffset}"
    }

    private fun provenance(context: String): JsonElement = buildJsonObject {
        put("backend", WORKER_NAME)
        put("backend_version", WORKER_VERSION)
        put("protocol_version", WORKER_PROTOCOL_VERSION)
        put("analysis_options", context)
    }

    private fun modifiers(declaration: org.jetbrains.kotlin.psi.KtModifierListOwner): List<String> =
        declaration.modifierList?.children
            ?.filterNot { child -> child is PsiWhiteSpace || child is KtAnnotationEntry }
            ?.map { child -> child.text }
            ?.filter { text -> text.isNotBlank() }
            ?: emptyList()

    private fun publicApiFingerprint(symbols: List<SymbolFact>): String = fingerprint(
        symbols.map { symbol ->
            val record = symbol.json.jsonObject
            val modifiers = record["modifiers"]!!.jsonArray.map { it.jsonPrimitive.content }
            val visible = "private" !in modifiers && "internal" !in modifiers
            if (visible) record.toString().encodeToByteArray() else ByteArray(0)
        }.filter { it.isNotEmpty() },
    )

    private fun fingerprint(bytes: ByteArray): String = fingerprint(listOf(bytes))

    private fun fingerprint(parts: List<ByteArray>): String {
        val digest = MessageDigest.getInstance("SHA-256")
        parts.forEach { digest.update(it.size.toLong().toString().encodeToByteArray()); digest.update(0); digest.update(it) }
        return "sha256:${digest.digest().joinToString("") { byte -> "%02x".format(byte) }}"
    }

    private fun stringArray(values: List<String>): JsonElement = buildJsonArray {
        values.forEach { add(JsonPrimitive(it)) }
    }

    private data class SymbolFact(val nameStart: Int, val nameEnd: Int, val json: JsonElement)
}

internal fun kotlinx.serialization.json.JsonObject.requiredString(name: String): String =
    this[name]?.jsonPrimitive?.content ?: error("missing $name")
