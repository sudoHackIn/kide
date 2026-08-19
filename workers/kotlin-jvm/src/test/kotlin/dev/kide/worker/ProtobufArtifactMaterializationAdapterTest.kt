package dev.kide.worker

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

class ProtobufArtifactMaterializationAdapterTest {
    private val descriptor = buildJsonObject {
        put("source_unit", buildJsonObject {
            put("id", "jvm:artifact"); put("component", "gradle:main")
            put("path", ".kide/dependencies/artifact"); put("language", "java"); put("origin", "dependency")
            put("content", "sha256:artifact"); put("context", "sha256:context")
        })
        put("provenance", buildJsonObject {
            put("backend", WORKER_NAME); put("backend_version", WORKER_VERSION)
            put("protocol_version", WORKER_PROTOCOL_VERSION); put("analysis_options", "sha256:options")
        })
        put("symbol_locators", buildJsonArray { })
    }

    @Test
    fun stagingCompletionAndErrorRoundTripWithoutBlobBytes() {
        val request = buildJsonObject {
            put("workspace_root", "."); put("artifact", descriptor)
            put("staging_directory", "staging-7"); put("blob_format_version", 1)
            put("artifact_locator", JsonNull)
        }
        val response = buildJsonObject {
            put("staged_filename", "artifact.kide"); put("byte_length", 12)
            put("sha256", "sha256:" + "ab".repeat(32)); put("blob_format_version", 1)
            put("timings", buildJsonArray { }); put("metrics", buildJsonArray { })
        }
        val error = buildJsonObject {
            put("code", "analysis_failed"); put("message", "staging failed"); put("retryable", true)
            put("supported_protocol_version", 3); put("received_protocol_version", JsonNull)
        }

        assertEquals(request, ProtobufArtifactMaterializationAdapter.json(ProtobufArtifactMaterializationAdapter.request(request)))
        assertEquals(response, ProtobufArtifactMaterializationAdapter.json(ProtobufArtifactMaterializationAdapter.response(response)))
        assertEquals(error, ProtobufArtifactMaterializationAdapter.json(ProtobufArtifactMaterializationAdapter.error(error)))
    }

    @Test
    fun malformedCompletionIsRejected() {
        val bad = buildJsonObject {
            put("staged_filename", "artifact.kide"); put("byte_length", 1)
            put("sha256", "sha256:bad"); put("blob_format_version", 1)
        }
        assertFailsWith<IllegalArgumentException> { ProtobufArtifactMaterializationAdapter.response(bad) }
    }

    @Test
    fun unknownErrorCodeIsRejected() {
        val bad = buildJsonObject {
            put("code", "unknown"); put("message", "nope"); put("retryable", false)
            put("supported_protocol_version", 3); put("received_protocol_version", JsonNull)
        }
        assertFailsWith<IllegalArgumentException> { ProtobufArtifactMaterializationAdapter.error(bad) }
    }
}
