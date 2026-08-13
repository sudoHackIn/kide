package dev.kide.worker

import java.nio.file.Files
import java.nio.file.Path
import org.jetbrains.kotlin.cli.common.ExitCode
import org.jetbrains.kotlin.cli.jvm.K2JVMCompiler

/** Runs the standalone K2 CLI with the FIR collector loaded as a compiler plugin. */
internal object K2SemanticExtractor {
    fun resolvedReferences(sourceFiles: List<Path>): List<K2ResolvedReference> {
        require(sourceFiles.isNotEmpty()) { "K2 analysis requires at least one Kotlin source file" }
        KideFirCollector.reset()
        val output = Files.createTempDirectory("kide-k2-output-")
        try {
            val plugin = pluginJar()
            val arguments = buildList {
                add("-Xplugin=$plugin")
                add("-no-stdlib")
                add("-no-reflect")
                add("-classpath")
                add(kotlinStdlib().toString())
                add("-d")
                add(output.toString())
                sourceFiles.sorted().forEach { add(it.toAbsolutePath().normalize().toString()) }
            }
            val exit = K2JVMCompiler().exec(System.err, *arguments.toTypedArray())
            check(exit == ExitCode.OK) { "K2 semantic analysis failed with $exit" }
            return KideFirCollector.snapshot()
        } finally {
            Files.walk(output).use { paths -> paths.sorted(Comparator.reverseOrder()).forEach(Files::deleteIfExists) }
        }
    }

    private fun pluginJar(): Path {
        val codeSource = Path.of(KideFirPluginRegistrar::class.java.protectionDomain.codeSource.location.toURI())
        if (codeSource.fileName.toString().endsWith(".jar")) return codeSource
        return Files.list(Path.of(System.getProperty("user.dir"), "build", "libs")).use { paths ->
            paths.filter { it.fileName.toString().endsWith(".jar") }.findFirst().orElseThrow {
                IllegalStateException("K2 collector plugin jar is not built")
            }
        }
    }

    private fun kotlinStdlib(): Path {
        val location = Path.of(kotlin.Unit::class.java.protectionDomain.codeSource.location.toURI())
        check(Files.isRegularFile(location)) { "Kotlin standard library is not a jar: $location" }
        return location
    }
}
