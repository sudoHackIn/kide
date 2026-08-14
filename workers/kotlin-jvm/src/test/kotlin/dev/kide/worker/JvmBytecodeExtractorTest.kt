package dev.kide.worker

import java.nio.file.Files
import java.nio.file.Path
import javax.tools.ToolProvider
import kotlin.io.path.createDirectories
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

class JvmBytecodeExtractorTest {
    @Test
    fun extracts_binary_declarations_overloads_and_hierarchy() {
        val classes = Files.createTempDirectory("kide-bytecode-")
        compile(classes, "fixture/Base.java", """
            package fixture;
            public class Base {}
        """)
        compile(classes, "fixture/Child.java", """
            package fixture;
            public class Child extends Base implements Runnable {
                public String title;
                public Child() {}
                public void run() {}
                public void overload() {}
                public void overload(int count) {}
            }
        """)

        val snapshots = JvmBytecodeExtractor.extract(
            classes,
            component = "gradle:app:main",
            context = "sha256:test-context",
        ).map { it.jsonObject }
        val child = snapshots.single { snapshot ->
            snapshot["source_unit"]!!.jsonObject["path"]!!.jsonPrimitive.content.endsWith("fixture/Child.class")
        }
        val sourceUnit = child["source_unit"]!!.jsonObject
        val symbols = child["symbols"]!!.jsonArray.map { it.jsonObject }

        assertEquals("dependency", sourceUnit["origin"]!!.jsonPrimitive.content)
        assertTrue(sourceUnit["id"]!!.jsonPrimitive.content.startsWith("jvm:sha256:"))
        assertTrue(symbols.any { it["kind"]!!.jsonPrimitive.content == "class" && it["qualified_name"]!!.jsonPrimitive.content == "fixture.Child" })
        assertTrue(symbols.any { it["kind"]!!.jsonPrimitive.content == "field" && it["name"]!!.jsonPrimitive.content == "title" })
        val overloads = symbols.filter { it["kind"]!!.jsonPrimitive.content == "method" && it["name"]!!.jsonPrimitive.content == "overload" }
        assertEquals(2, overloads.size)
        assertEquals(2, overloads.map { it["signature"]!!.jsonPrimitive.content }.toSet().size)
        assertTrue(child["hierarchy"]!!.jsonArray.any { edge ->
            edge.jsonObject["supertype"]!!.jsonPrimitive.content.endsWith(":fixture.Base")
        })
        assertTrue(child["hierarchy"]!!.jsonArray.any { edge ->
            edge.jsonObject["supertype"]!!.jsonPrimitive.content.endsWith(":java.lang.Runnable")
        })
    }

    @Test
    fun mapsOnlyUnambiguousK2TargetsToDurableDependencySymbols() {
        val classes = Files.createTempDirectory("kide-bytecode-k2-map-")
        compile(classes, "fixture/Api.java", """
            package fixture;
            public class Api { public void unique() {} public void overload() {} public void overload(int count) {} }
        """)

        val targets = JvmBytecodeExtractor.resolvedTargetIds(
            classpath = listOf(classes),
            targetKeys = setOf("callable:fixture/Api.unique", "callable:fixture/Api.overload"),
        )

        assertTrue(targets.getValue("callable:fixture/Api.unique").endsWith(":fixture.Api#method:unique()V"))
        assertTrue("callable:fixture/Api.overload" !in targets)
    }

    private fun compile(output: Path, relativeSource: String, source: String) {
        val file = output.resolve(relativeSource)
        file.parent.createDirectories()
        Files.writeString(file, source.trimIndent())
        val compiler = checkNotNull(ToolProvider.getSystemJavaCompiler()) { "JDK compiler is available" }
        assertEquals(
            0,
            compiler.run(null, null, null, "-classpath", output.toString(), "-d", output.toString(), file.toString()),
        )
    }
}
