package dev.kide.worker

import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

class ProjectManifestProtocolTest {
    @Test
    fun projectManifestRequestUsesTheVersionedWorkerEnvelope() {
        val root = Files.createTempDirectory("kide-gradle-protocol-")
        Files.writeString(root.resolve("settings.gradle.kts"), "rootProject.name = \"fixture\"")
        Files.writeString(root.resolve("build.gradle.kts"), "plugins { java }")

        val response = dispatch(
            WorkerEnvelope(
                protocolVersion = WORKER_PROTOCOL_VERSION,
                requestId = "manifest-1",
                kind = WorkerMessageKind.PROJECT_MANIFEST_REQUEST,
                payload = buildJsonObject { put("workspace_root", root.toString()) },
            ),
        )

        assertEquals(WorkerMessageKind.PROJECT_MANIFEST_RESPONSE, response.kind)
        assertEquals("manifest-1", response.requestId)
        val manifest = response.payload.jsonObject["manifest"]!!.jsonObject
        assertEquals("gradle", manifest["components"]!!.jsonArray.single().jsonObject["build_system"]!!.jsonPrimitive.content)
        assertTrue(manifest["fingerprint"]!!.jsonPrimitive.content.startsWith("sha256:"))
    }
}
