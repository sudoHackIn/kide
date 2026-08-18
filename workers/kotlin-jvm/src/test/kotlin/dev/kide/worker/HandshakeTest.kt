package dev.kide.worker

import kotlin.test.Test
import kotlin.test.assertEquals
import kide.worker.v1.Worker

class HandshakeTest {
    @Test
    fun handshakeIsAVersionedProtocolEnvelopeWithStaticCapabilities() {
        val envelope = handshakeEnvelope()

        assertEquals(WORKER_PROTOCOL_VERSION, envelope.protocolVersion)
        assertEquals("handshake", envelope.requestId)
        assertEquals(Worker.Envelope.MessageCase.HANDSHAKE_RESPONSE, envelope.messageCase)
        assertEquals("kide-kotlin-jvm", envelope.handshakeResponse.backend)
        assertEquals(
            listOf("handshake", "project_manifest", "file_analysis_snapshot", "dependency_analysis"),
            envelope.handshakeResponse.capabilitiesList,
        )
        assertEquals(listOf("kotlin", "java"), envelope.handshakeResponse.languagesList)
        assertEquals(
            listOf("applications.resolved_target", "hierarchy.direct"),
            envelope.handshakeResponse.semanticQueryCapabilitiesList.map { it.name },
        )
        assertEquals(
            listOf("normalized_facts", "candidate_symbols"),
            envelope.handshakeResponse.semanticQueryCapabilitiesList.map { it.resultKind },
        )
    }
}
