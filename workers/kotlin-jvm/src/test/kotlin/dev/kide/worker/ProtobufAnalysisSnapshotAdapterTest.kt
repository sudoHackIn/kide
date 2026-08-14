package dev.kide.worker

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

class ProtobufAnalysisSnapshotAdapterTest {
    @Test
    fun fixtureSnapshotBecomesTypedProtobufFacts() {
        val fixture = requireNotNull(javaClass.classLoader.getResource("analysis-batch-response.json"))
            .readText()
        val payload = Json.parseToJsonElement(fixture).jsonObject["payload"]!!.jsonObject
        val snapshot = payload["snapshots"]!!.jsonArray.first().jsonObject

        assertEquals(
            payload,
            ProtobufAnalysisSnapshotAdapter.json(
                ProtobufAnalysisSnapshotAdapter.analysisBatchResponse(payload),
            ),
        )

        val protobuf = ProtobufAnalysisSnapshotAdapter.snapshot(snapshot)

        assertEquals("gradle:app:main:src/main/kotlin/PaymentService.kt", protobuf.sourceUnit.id)
        assertEquals("kide-kotlin-jvm", protobuf.provenancesList.single().backend)
        assertEquals("kotlin:demo.PaymentService", protobuf.symbolsList.single().id)
        assertEquals("gradle:app:main", protobuf.symbolsList.single().componentId)
        val restored = ProtobufAnalysisSnapshotAdapter.json(protobuf)
        assertEquals("gradle:app:main:src/main/kotlin/PaymentService.kt", restored["source_unit"]!!.jsonObject["id"]!!.jsonPrimitive.content)
        assertEquals("kide-kotlin-jvm", restored["provenance"]!!.jsonObject["backend"]!!.jsonPrimitive.content)
        assertEquals("kotlin:demo.PaymentService", restored["symbols"]!!.jsonArray.single().jsonObject["id"]!!.jsonPrimitive.content)
    }

    @Test
    fun everySnapshotFactTableRoundTrips() {
        val provenance = buildJsonObject {
            put("backend", "kide-kotlin-jvm"); put("backend_version", "1.0")
            put("protocol_version", 3); put("analysis_options", "sha256:options")
        }
        val source = buildJsonObject {
            put("id", "unit"); put("component", "component"); put("path", "src/Main.kt")
            put("language", "kotlin"); put("origin", "source")
            put("content", "sha256:content"); put("context", "sha256:context")
        }
        val occurrence = buildJsonObject {
            put("range", buildJsonObject { put("source_unit", "unit"); put("bytes", buildJsonObject { put("start", 10); put("end", 14) }) })
            put("kind", "call"); put("enclosing_symbol", "caller"); put("target", "target")
            put("type_id", "type"); put("precision", "exact"); put("freshness", "fresh")
            put("completeness", "complete"); put("provenance", provenance)
        }
        val snapshot = buildJsonObject {
            put("source_unit", source); put("structural_fingerprint", "sha256:structure")
            put("public_api_fingerprint", "sha256:api")
            put("symbols", buildJsonArray { add(buildJsonObject {
                put("id", "caller"); put("backend_key", buildJsonObject { put("backend", "kide-kotlin-jvm"); put("schema_version", 1); put("value", "caller-key") })
                put("language", "kotlin"); put("kind", "function"); put("name", "caller")
                put("qualified_name", "demo.caller"); put("signature", JsonNull); put("component", "component")
                put("declaration", buildJsonObject { put("source_unit", "unit"); put("bytes", buildJsonObject { put("start", 0); put("end", 6) }) })
                put("name_range", buildJsonObject { put("source_unit", "unit"); put("bytes", buildJsonObject { put("start", 0); put("end", 6) }) })
                put("owner", JsonNull); put("modifiers", buildJsonArray { add(JsonPrimitive("public")) }); put("annotations", buildJsonArray { })
                put("freshness", "fresh"); put("completeness", "complete"); put("provenance", provenance)
            }) })
            put("occurrences", buildJsonArray { add(occurrence) })
            put("references", buildJsonArray { add(buildJsonObject { put("source", occurrence); put("target", "target"); put("precision", "exact") }) })
            put("calls", buildJsonArray { add(buildJsonObject { put("source", occurrence); put("target", "target"); put("caller", "caller"); put("precision", "exact") }) })
            put("hierarchy", buildJsonArray { add(buildJsonObject { put("subtype", "child"); put("supertype", "parent"); put("precision", "exact"); put("provenance", provenance) }) })
            put("types", buildJsonArray { add(buildJsonObject {
                put("id", "type"); put("language", "kotlin"); put("display", "String")
                put("backend_key", buildJsonObject { put("backend", "kide-kotlin-jvm"); put("schema_version", 2); put("value", "kotlin.String") })
                put("freshness", "fresh"); put("completeness", "complete"); put("provenance", provenance)
            }) })
            put("diagnostics", buildJsonArray { add(buildJsonObject {
                put("source_unit", "unit"); put("range", buildJsonObject { put("start", 20); put("end", 25) })
                put("severity", "warning"); put("code", "W1"); put("message", "warning")
                put("freshness", "fresh"); put("completeness", "complete"); put("provenance", provenance)
            }) })
            put("completeness", "complete"); put("provenance", provenance)
        }

        assertEquals(snapshot, ProtobufAnalysisSnapshotAdapter.json(ProtobufAnalysisSnapshotAdapter.snapshot(snapshot)))
    }
}
