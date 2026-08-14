package dev.kide.worker

import kotlin.test.Test
import kotlin.test.assertEquals
import kide.worker.v1.Worker

/** Shared descriptor field names/types, generated from protocol/kide/worker.proto. */
class ProtobufDescriptorContractTest {
    @Test
    fun descriptorUsesTheRepositoryOwnedGeneratedContract() {
        val descriptor = Worker.ArtifactDescriptor.newBuilder()
            .setSourceUnitId("jar:example")
            .setComponentId("gradle:app:main")
            .setWorkspacePath(".gradle/cache/example.jar")
            .setContentFingerprint("sha256:content")
            .setContextFingerprint("sha256:context")
            .setBackend("kide-kotlin-jvm")
            .setBackendVersion("0.1.0")
            .setWorkerProtocolVersion(WORKER_PROTOCOL_VERSION)
            .setAnalysisOptionsFingerprint("sha256:options")
            .setLanguage("java")
            .setOrigin("dependency")
            .build()

        val restored = Worker.ArtifactDescriptor.parseFrom(descriptor.toByteArray())
        assertEquals(descriptor, restored)
    }
}
