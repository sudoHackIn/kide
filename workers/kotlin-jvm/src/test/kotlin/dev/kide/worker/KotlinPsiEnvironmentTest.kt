package dev.kide.worker

import org.jetbrains.kotlin.cli.jvm.compiler.KotlinCoreEnvironment
import org.jetbrains.kotlin.cli.jvm.compiler.EnvironmentConfigFiles
import org.jetbrains.kotlin.K1Deprecation
import org.jetbrains.kotlin.config.CompilerConfiguration
import org.jetbrains.kotlin.psi.KtPsiFactory
import org.jetbrains.kotlin.com.intellij.openapi.util.Disposer
import kotlin.test.Test
import kotlin.test.assertEquals

class KotlinPsiEnvironmentTest {
    @Test
    @OptIn(K1Deprecation::class, CompilerConfiguration.Internals::class)
    fun createsKotlinPsiWithoutAnIdeProcess() {
        val disposable = Disposer.newDisposable("kide-psi-test")
        try {
            val environment = KotlinCoreEnvironment.createForProduction(
                disposable,
                CompilerConfiguration(),
                EnvironmentConfigFiles.JVM_CONFIG_FILES,
            )
            val file = KtPsiFactory(environment.project, false)
                .createFile("Example.kt", "package fixture\nclass Example")

            assertEquals("fixture", file.packageFqName.asString())
            assertEquals("Example", file.declarations.single().name)
        } finally {
            Disposer.dispose(disposable)
        }
    }
}
