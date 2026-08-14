package dev.kide.worker

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

class ProtobufArtifactDiscoveryAdapterTest {
    @Test
    fun descriptorRoundTripsWithoutChangingCanonicalIdentity() {
        val original = buildJsonObject {
            put("source_unit", buildJsonObject {
                put("id", "jvm:sha256:artifact"); put("component", "gradle:main")
                put("path", ".kide/dependencies/artifact"); put("content", "sha256:artifact"); put("context", "sha256:context")
            })
            put("provenance", buildJsonObject {
                put("backend", WORKER_NAME); put("backend_version", WORKER_VERSION)
                put("protocol_version", WORKER_PROTOCOL_VERSION); put("analysis_options", "sha256:options")
            })
        }
        val protobuf = ProtobufArtifactDiscoveryAdapter.descriptor(original)
        val restored = ProtobufArtifactDiscoveryAdapter.json(protobuf)
        assertEquals("jvm:sha256:artifact", restored.jsonObject["source_unit"]!!.jsonObject["id"]!!.jsonPrimitive.content)
        assertEquals(WORKER_VERSION, restored.jsonObject["provenance"]!!.jsonObject["backend_version"]!!.jsonPrimitive.content)
    }
}
