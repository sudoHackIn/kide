package dev.kide.worker

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

class HandshakeTest {
    @Test
    fun handshakeIsAVersionedProtocolEnvelopeWithStaticCapabilities() {
        val envelope = protocolJson.decodeFromString<WorkerEnvelope>(handshakeJson())

        assertEquals(WORKER_PROTOCOL_VERSION, envelope.protocolVersion)
        assertEquals("handshake", envelope.requestId)
        assertEquals(WorkerMessageKind.HANDSHAKE_RESPONSE, envelope.kind)
        assertEquals("kide-kotlin-jvm", envelope.payload.jsonObject["capabilities"]!!.jsonObject["identity"]!!.jsonObject["backend"]!!.jsonPrimitive.content)
        assertEquals("handshake", envelope.payload.jsonObject["capabilities"]!!.jsonObject["capabilities"]!!.jsonArray.single().jsonPrimitive.content)
    }

    @Test
    fun compatible_version_has_no_error() {
        assertNull(validateProtocolVersion(WORKER_PROTOCOL_VERSION))
    }

    @Test
    fun incompatible_version_has_structured_non_retryable_error() {
        val error = validateProtocolVersion(WORKER_PROTOCOL_VERSION + 1)!!

        assertEquals("incompatible_protocol_version", error.code)
        assertEquals(false, error.retryable)
        assertEquals(WORKER_PROTOCOL_VERSION + 1, error.receivedProtocolVersion)
    }
}
