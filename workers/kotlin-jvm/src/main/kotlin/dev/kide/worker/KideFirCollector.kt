package dev.kide.worker

import java.util.concurrent.ConcurrentLinkedQueue
import org.jetbrains.kotlin.KtSourceElement
import org.jetbrains.kotlin.compiler.plugin.CompilerPluginRegistrar
import org.jetbrains.kotlin.compiler.plugin.ExperimentalCompilerApi
import org.jetbrains.kotlin.compiler.plugin.registerExtension as registerFirExtension
import org.jetbrains.kotlin.config.CompilerConfiguration
import org.jetbrains.kotlin.diagnostics.DiagnosticReporter
import org.jetbrains.kotlin.fir.FirSession
import org.jetbrains.kotlin.fir.analysis.checkers.MppCheckerKind
import org.jetbrains.kotlin.fir.analysis.checkers.context.CheckerContext
import org.jetbrains.kotlin.fir.analysis.checkers.expression.ExpressionCheckers
import org.jetbrains.kotlin.fir.analysis.checkers.expression.FirExpressionChecker
import org.jetbrains.kotlin.fir.analysis.extensions.FirAdditionalCheckersExtension
import org.jetbrains.kotlin.fir.expressions.FirQualifiedAccessExpression
import org.jetbrains.kotlin.fir.extensions.FirExtensionRegistrar
import org.jetbrains.kotlin.fir.references.FirResolvedNamedReference
import org.jetbrains.kotlin.fir.symbols.FirBasedSymbol
import org.jetbrains.kotlin.fir.symbols.impl.FirCallableSymbol
import org.jetbrains.kotlin.fir.symbols.impl.FirClassSymbol

/**
 * A K2 compiler plugin extension point. It observes FIR after resolution and
 * records only genuine compiler-selected targets; it never performs a textual
 * lookup. The worker drains this short-lived collector after each CLI run.
 */
internal object KideFirCollector {
    private val references = ConcurrentLinkedQueue<K2ResolvedReference>()

    fun reset() = references.clear()

    fun snapshot(): List<K2ResolvedReference> = references.toList().sortedWith(
        compareBy({ it.sourcePath }, { it.startUtf16 }, { it.endUtf16 }, { it.targetKey }),
    )

    internal fun record(sourcePath: String, source: KtSourceElement, target: FirBasedSymbol<*>, call: Boolean) {
        references.add(
            K2ResolvedReference(
                sourcePath = sourcePath,
                startUtf16 = source.startOffset,
                endUtf16 = source.endOffset,
                targetKey = targetKey(target),
                isCall = call,
            ),
        )
    }

    private fun targetKey(symbol: FirBasedSymbol<*>): String = when (symbol) {
        is FirClassSymbol<*> -> "class:${symbol.classId.asSingleFqName().asString()}"
        is FirCallableSymbol<*> -> "callable:${symbol.callableIdAsString()}"
        else -> "fir:${symbol::class.qualifiedName}:${symbol.source?.startOffset ?: -1}"
    }
}

internal data class K2ResolvedReference(
    val sourcePath: String,
    val startUtf16: Int,
    val endUtf16: Int,
    val targetKey: String,
    val isCall: Boolean,
)

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
    override val expressionCheckers: ExpressionCheckers = object : ExpressionCheckers() {
        override val qualifiedAccessExpressionCheckers: Set<FirExpressionChecker<FirQualifiedAccessExpression>> =
            setOf(KideQualifiedAccessChecker)
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
        )
    }
}
