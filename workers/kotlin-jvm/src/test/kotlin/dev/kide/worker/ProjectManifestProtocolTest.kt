package dev.kide.worker

import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue
import kide.worker.v1.Worker

class ProjectManifestProtocolTest {
    @Test
    fun projectManifestRequestUsesTheVersionedWorkerEnvelope() {
        val root = Files.createTempDirectory("kide-gradle-protocol-")
        Files.writeString(root.resolve("settings.gradle.kts"), "rootProject.name = \"fixture\"")
        Files.writeString(root.resolve("build.gradle.kts"), "plugins { java }")

        val response = dispatch(
            Worker.Envelope.newBuilder()
                .setProtocolVersion(WORKER_PROTOCOL_VERSION)
                .setRequestId("manifest-1")
                .setProjectManifestRequest(Worker.ProjectManifestRequest.newBuilder().setWorkspaceRoot(root.toString()))
                .build(),
        )

        assertEquals(Worker.Envelope.MessageCase.PROJECT_MANIFEST_RESPONSE, response.messageCase)
        assertEquals("manifest-1", response.requestId)
        val manifest = response.projectManifestResponse.manifest
        assertEquals("gradle", manifest.componentsList.single().buildSystem)
        assertTrue(manifest.fingerprint.startsWith("sha256:"))
    }
}
