package dev.kide.worker

import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put

class KotlinStructuralBatchTest {
    @Test
    fun analyzesMultipleKotlinSourceUnitsInOneWorkerBatch() {
        val root = Files.createTempDirectory("kide-psi-batch-")
        Files.writeString(root.resolve("One.kt"), "package fixture\nclass One")
        Files.writeString(root.resolve("Two.kt"), "package fixture\nfun create() = One()")
        val payload = buildJsonObject {
            put("source_units", buildJsonArray {
                add(sourceUnit("One.kt"))
                add(sourceUnit("Two.kt"))
            })
        }

        val result = structuralBatch(payload, root)

        assertEquals(2, result.jsonObject["snapshots"]!!.jsonArray.size)
        val use = result.jsonObject["snapshots"]!!.jsonArray
            .map { it.jsonObject }
            .single { it["source_unit"]!!.jsonObject["path"]!!.toString().contains("Two.kt") }
        assertEquals(1, use["calls"]!!.jsonArray.size)
        val call = use["calls"]!!.jsonArray.single().jsonObject
        val target = call["target"]!!.toString()
        assertTrue(target.contains("One.kt") && target.contains("constructor"))
        assertTrue(call["caller"]!!.toString().contains("Two.kt") && call["caller"]!!.toString().contains("function:create"))
    }

    @Test
    fun mapsTheExactSelectedSourceOverload() {
        val root = Files.createTempDirectory("kide-k2-overload-")
        Files.writeString(root.resolve("Overload.kt"), """
            package fixture
            fun pick(value: Int) = value
            fun pick(value: String) = value
            fun use() = pick(1)
        """.trimIndent())
        val payload = buildJsonObject {
            put("source_units", buildJsonArray { add(sourceUnit("Overload.kt")) })
        }

        val snapshot = structuralBatch(payload, root).jsonObject["snapshots"]!!.jsonArray.single().jsonObject

        val call = snapshot["calls"]!!.jsonArray.single().jsonObject
        val expected = snapshot["symbols"]!!.jsonArray.map { it.jsonObject }.single { symbol ->
            symbol["name"]!!.toString().contains("pick") && symbol["signature"]!!.toString().contains("Int")
        }["id"]!!.toString()
        assertEquals(expected, call["target"]!!.toString())
        val typeId = call["source"]!!.jsonObject["type_id"]
        assertTrue(typeId != null && typeId.toString().contains("kotlin:type:"))
        assertTrue(snapshot["types"]!!.jsonArray.any { type -> type.jsonObject["id"] == typeId })
    }

    @Test
    fun emitsOnlyDirectK2HierarchyEdges() {
        val root = Files.createTempDirectory("kide-k2-hierarchy-")
        Files.writeString(root.resolve("Hierarchy.kt"), """
            package fixture
            interface Parent
            open class Base : Parent
            class Child : Base()
        """.trimIndent())
        val payload = buildJsonObject {
            put("source_units", buildJsonArray { add(sourceUnit("Hierarchy.kt")) })
        }

        val snapshot = structuralBatch(payload, root).jsonObject["snapshots"]!!.jsonArray.single().jsonObject
        val symbols = snapshot["symbols"]!!.jsonArray.map { it.jsonObject }
        val child = symbols.single { it["name"]!!.toString().contains("Child") }["id"]!!.toString()
        val base = symbols.single { it["name"]!!.toString().contains("Base") }["id"]!!.toString()
        val parent = symbols.single { it["name"]!!.toString().contains("Parent") }["id"]!!.toString()
        val edges = snapshot["hierarchy"]!!.jsonArray.map { it.jsonObject }
        assertTrue(edges.any { it["subtype"]!!.toString() == child && it["supertype"]!!.toString() == base })
        assertTrue(edges.none { it["subtype"]!!.toString() == child && it["supertype"]!!.toString() == parent })
    }

    @Test
    fun resolvesExtensionAndImplicitReceiverCallsThroughK2() {
        val root = Files.createTempDirectory("kide-k2-receivers-")
        Files.writeString(root.resolve("Receivers.kt"), """
            package fixture

            typealias Label = String
            fun Label.decorate() = "[$this]"

            class Formatter {
                fun suffix() = "!"
                fun render(): String = "value".decorate() + suffix()
            }
        """.trimIndent())
        val payload = buildJsonObject {
            put("source_units", buildJsonArray { add(sourceUnit("Receivers.kt")) })
        }

        val snapshot = structuralBatch(payload, root).jsonObject["snapshots"]!!.jsonArray.single().jsonObject
        val symbols = snapshot["symbols"]!!.jsonArray.map { it.jsonObject }
        val decorate = symbols.single { it["name"]!!.toString().contains("decorate") }["id"]!!.toString()
        val suffix = symbols.single { it["name"]!!.toString().contains("suffix") }["id"]!!.toString()
        val calls = snapshot["calls"]!!.jsonArray.map { it.jsonObject }
        assertTrue(calls.any { it["target"]!!.toString() == decorate })
        assertTrue(calls.any { it["target"]!!.toString() == suffix })
    }

    @Test
    fun emitsExactMethodOverrideAsHierarchyEdge() {
        val root = Files.createTempDirectory("kide-k2-overrides-")
        Files.writeString(root.resolve("Overrides.kt"), """
            package fixture

            interface Parent { fun process(value: String): String }
            class Child : Parent {
                override fun process(value: String) = value.uppercase()
            }
        """.trimIndent())
        val payload = buildJsonObject {
            put("source_units", buildJsonArray { add(sourceUnit("Overrides.kt")) })
        }

        val snapshot = structuralBatch(payload, root).jsonObject["snapshots"]!!.jsonArray.single().jsonObject
        val symbols = snapshot["symbols"]!!.jsonArray.map { it.jsonObject }
        val methods = symbols.filter { it["name"]!!.toString().contains("process") }
        val parentMethod = methods.single { it["qualified_name"]!!.toString().contains("Parent.process") }["id"]!!.toString()
        val childMethod = methods.single { it["qualified_name"]!!.toString().contains("Child.process") }["id"]!!.toString()
        val edges = snapshot["hierarchy"]!!.jsonArray.map { it.jsonObject }
        assertTrue(edges.any { it["subtype"]!!.toString() == childMethod && it["supertype"]!!.toString() == parentMethod })
    }

    @Test
    fun resolvesGenericCallsAndAliasConstructorToTheirDeclarations() {
        val root = Files.createTempDirectory("kide-k2-generic-alias-")
        Files.writeString(root.resolve("GenericAlias.kt"), """
            package fixture

            class RealService
            typealias Service = RealService
            fun <T> echo(value: T): T = value
            fun use(): Service {
                echo(42)
                return Service()
            }
        """.trimIndent())
        val payload = buildJsonObject {
            put("source_units", buildJsonArray { add(sourceUnit("GenericAlias.kt")) })
        }

        val snapshot = structuralBatch(payload, root).jsonObject["snapshots"]!!.jsonArray.single().jsonObject
        val symbols = snapshot["symbols"]!!.jsonArray.map { it.jsonObject }
        val echo = symbols.single { it["name"]!!.toString().contains("echo") }["id"]!!.toString()
        val constructor = symbols.single { it["kind"]!!.toString().contains("constructor") }["id"]!!.toString()
        val calls = snapshot["calls"]!!.jsonArray.map { it.jsonObject }
        assertTrue(calls.any { it["target"]!!.toString() == echo })
        assertTrue(calls.any { it["target"]!!.toString() == constructor })
    }

    private fun sourceUnit(path: String) = buildJsonObject {
        put("id", "gradle::fixture:main:$path")
        put("component", "gradle::fixture:main")
        put("path", path)
        put("language", "kotlin")
        put("origin", "source")
        put("content", "sha256:test")
        put("context", "sha256:context")
    }
}
