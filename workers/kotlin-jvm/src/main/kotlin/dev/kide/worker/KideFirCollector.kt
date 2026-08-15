package dev.kide.worker

import java.util.concurrent.ConcurrentLinkedQueue
import org.jetbrains.kotlin.KtSourceElement
import org.jetbrains.kotlin.KtFakeSourceElementKind
import org.jetbrains.kotlin.compiler.plugin.CompilerPluginRegistrar
import org.jetbrains.kotlin.compiler.plugin.ExperimentalCompilerApi
import org.jetbrains.kotlin.compiler.plugin.registerExtension as registerFirExtension
import org.jetbrains.kotlin.config.CompilerConfiguration
import org.jetbrains.kotlin.diagnostics.DiagnosticReporter
import org.jetbrains.kotlin.fir.FirSession
import org.jetbrains.kotlin.fir.analysis.checkers.MppCheckerKind
import org.jetbrains.kotlin.fir.analysis.checkers.findClosestClassOrObject
import org.jetbrains.kotlin.fir.analysis.checkers.overriddenFunctions
import org.jetbrains.kotlin.fir.analysis.checkers.context.CheckerContext
import org.jetbrains.kotlin.fir.analysis.checkers.expression.ExpressionCheckers
import org.jetbrains.kotlin.fir.analysis.checkers.expression.FirExpressionChecker
import org.jetbrains.kotlin.fir.analysis.checkers.type.FirTypeChecker
import org.jetbrains.kotlin.fir.analysis.checkers.type.TypeCheckers
import org.jetbrains.kotlin.fir.analysis.extensions.FirAdditionalCheckersExtension
import org.jetbrains.kotlin.fir.expressions.FirQualifiedAccessExpression
import org.jetbrains.kotlin.fir.declarations.FirFunction
import org.jetbrains.kotlin.fir.declarations.FirClass
import org.jetbrains.kotlin.fir.declarations.FirNamedFunction
import org.jetbrains.kotlin.fir.analysis.checkers.declaration.DeclarationCheckers
import org.jetbrains.kotlin.fir.analysis.checkers.declaration.FirDeclarationChecker
import org.jetbrains.kotlin.fir.types.FirResolvedTypeRef
import org.jetbrains.kotlin.fir.types.resolvedType
import org.jetbrains.kotlin.fir.types.ConeClassLikeType
import org.jetbrains.kotlin.fir.extensions.FirExtensionRegistrar
import org.jetbrains.kotlin.fir.references.FirResolvedNamedReference
import org.jetbrains.kotlin.fir.symbols.FirBasedSymbol
import org.jetbrains.kotlin.fir.symbols.SymbolInternals
import org.jetbrains.kotlin.fir.symbols.impl.FirCallableSymbol
import org.jetbrains.kotlin.fir.symbols.impl.FirClassSymbol

/**
 * A K2 compiler plugin extension point. It observes FIR after resolution and
 * records only genuine compiler-selected targets; it never performs a textual
 * lookup. The worker drains this short-lived collector after each CLI run.
 */
@OptIn(SymbolInternals::class)
internal object KideFirCollector {
    private val references = ConcurrentLinkedQueue<K2ResolvedReference>()
    private val hierarchy = ConcurrentLinkedQueue<K2HierarchyEdge>()

    fun reset() { references.clear(); hierarchy.clear() }

    fun snapshot(): K2SemanticFacts = K2SemanticFacts(
        references = references.toList().sortedWith(compareBy({ it.sourcePath }, { it.startUtf16 }, { it.endUtf16 }, { it.targetKey })),
        hierarchy = hierarchy.toList().distinct().sortedWith(compareBy({ it.subtypeKey }, { it.supertypeKey })),
    )

    internal fun recordHierarchy(subtype: FirClass, supertype: ConeClassLikeType) {
        val subtypeKey = "class:${subtype.symbol.classId.asSingleFqName().asString()}"
        val supertypeKey = "class:${supertype.lookupTag.classId.asSingleFqName().asString()}"
        if (supertypeKey != "class:kotlin.Any") hierarchy.add(K2HierarchyEdge(subtypeKey, supertypeKey))
    }

    internal fun recordOverride(overriding: FirCallableSymbol<*>, overridden: FirCallableSymbol<*>) {
        hierarchy.add(K2HierarchyEdge(targetKey(overriding), targetKey(overridden)))
    }

    internal fun record(
        sourcePath: String,
        source: KtSourceElement,
        target: FirBasedSymbol<*>,
        call: Boolean,
        typeDisplay: String?,
    ) {
        references.add(
            K2ResolvedReference(
                sourcePath = sourcePath,
                startUtf16 = source.startOffset,
                endUtf16 = source.endOffset,
                targetKey = targetKey(target),
                isCall = call,
                typeDisplay = typeDisplay,
            ),
        )
    }

    internal fun recordType(sourcePath: String, source: KtSourceElement, type: ConeClassLikeType) {
        references.add(
            K2ResolvedReference(
                sourcePath = sourcePath,
                startUtf16 = source.startOffset,
                endUtf16 = source.endOffset,
                targetKey = "class:${type.lookupTag.classId.asSingleFqName().asString()}",
                isCall = false,
                typeDisplay = type.toString(),
            ),
        )
    }

    private fun targetKey(symbol: FirBasedSymbol<*>): String = when (symbol) {
        is FirClassSymbol<*> -> "class:${symbol.classId.asSingleFqName().asString()}"
        is FirCallableSymbol<*> -> "callable:${symbol.callableIdAsString()}#${callableParameterTypes(symbol)}"
        else -> "fir:${symbol::class.qualifiedName}:${symbol.source?.startOffset ?: -1}"
    }

    private fun callableParameterTypes(symbol: FirCallableSymbol<*>): String = (symbol.fir as? FirFunction)
        ?.valueParameters
        ?.joinToString(prefix = "(", postfix = ")") { parameter ->
            (parameter.returnTypeRef as? FirResolvedTypeRef)?.coneType?.toString() ?: "?"
        }
        ?.replace(" ", "")
        ?: "(?)"
}

internal data class K2ResolvedReference(
    val sourcePath: String,
    val startUtf16: Int,
    val endUtf16: Int,
    val targetKey: String,
    val isCall: Boolean,
    val typeDisplay: String?,
)

internal data class K2HierarchyEdge(val subtypeKey: String, val supertypeKey: String)
internal data class K2SemanticFacts(val references: List<K2ResolvedReference>, val hierarchy: List<K2HierarchyEdge>)

/** Registered through the standard compiler-plugin service entry. */
@OptIn(ExperimentalCompilerApi::class)
internal class KideFirPluginRegistrar : CompilerPluginRegistrar() {
    override val pluginId: String = "dev.kide.fir-collector"
    override val supportsK2: Boolean = true

    override fun ExtensionStorage.registerExtensions(configuration: CompilerConfiguration) {
        with(FirExtensionRegistrar.Companion) { registerFirExtension(KideFirExtensionRegistrar()) }
    }
}

internal class KideFirExtensionRegistrar : FirExtensionRegistrar() {
    override fun ExtensionRegistrarContext.configurePlugin() {
        +::KideFirCheckersExtension
    }
}

internal class KideFirCheckersExtension(session: FirSession) : FirAdditionalCheckersExtension(session) {
    override val declarationCheckers: DeclarationCheckers = object : DeclarationCheckers() {
        override val classCheckers: Set<FirDeclarationChecker<FirClass>> = setOf(KideClassHierarchyChecker)
        override val simpleFunctionCheckers: Set<FirDeclarationChecker<FirNamedFunction>> = setOf(KideFunctionOverrideChecker)
    }
    override val expressionCheckers: ExpressionCheckers = object : ExpressionCheckers() {
        override val qualifiedAccessExpressionCheckers: Set<FirExpressionChecker<FirQualifiedAccessExpression>> =
            setOf(KideQualifiedAccessChecker)
    }
    override val typeCheckers: TypeCheckers = object : TypeCheckers() {
        override val resolvedTypeRefCheckers: Set<FirTypeChecker<FirResolvedTypeRef>> =
            setOf(KideResolvedTypeRefChecker)
    }
}

private object KideClassHierarchyChecker : FirDeclarationChecker<FirClass>(MppCheckerKind.Platform) {
    context(context: CheckerContext, reporter: DiagnosticReporter)
    override fun check(declaration: FirClass) {
        declaration.symbol.resolvedSuperTypes.filterIsInstance<ConeClassLikeType>().forEach { supertype ->
            KideFirCollector.recordHierarchy(declaration, supertype)
        }
    }
}

private object KideFunctionOverrideChecker : FirDeclarationChecker<FirNamedFunction>(MppCheckerKind.Platform) {
    context(context: CheckerContext, reporter: DiagnosticReporter)
    override fun check(declaration: FirNamedFunction) {
        val owner = context.findClosestClassOrObject() ?: return
        declaration.symbol.overriddenFunctions(owner).forEach { overridden ->
            KideFirCollector.recordOverride(declaration.symbol, overridden)
        }
    }
}

private object KideQualifiedAccessChecker : FirExpressionChecker<FirQualifiedAccessExpression>(MppCheckerKind.Platform) {
    context(context: CheckerContext, reporter: DiagnosticReporter)
    override fun check(expression: FirQualifiedAccessExpression) {
        val source = expression.source ?: return
        val target = (expression.calleeReference as? FirResolvedNamedReference)?.resolvedSymbol ?: return
        val sourcePath = context.containingFile?.path ?: return
        KideFirCollector.record(
            sourcePath = sourcePath,
            source = source,
            target = target,
            call = expression::class.simpleName?.contains("FunctionCall") == true,
            typeDisplay = expression.resolvedType.toString(),
        )
    }
}

private object KideResolvedTypeRefChecker : FirTypeChecker<FirResolvedTypeRef>(MppCheckerKind.Platform) {
    context(context: CheckerContext, reporter: DiagnosticReporter)
    override fun check(typeRef: FirResolvedTypeRef) {
        val type = typeRef.coneType as? ConeClassLikeType ?: return
        val sourcePath = context.containingFile?.path ?: return
        val source = typeRef.source ?: return
        // Class self types and inferred type arguments have synthetic source
        // ranges. Only real source elements are user-visible type references.
        if (source.kind is KtFakeSourceElementKind) return
        KideFirCollector.recordType(sourcePath, source, type)
    }
}
