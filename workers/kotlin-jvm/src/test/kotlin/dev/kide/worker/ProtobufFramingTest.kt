package dev.kide.worker

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kide.worker.v1.Worker

class ProtobufFramingTest {
    @Test
    fun roundTripsDelimitedHandshakeEnvelope() {
        val output = ByteArrayOutputStream()
        val expected = handshakeEnvelope("frame-1")

        ProtobufFraming.write(output, expected)

        assertEquals(expected, ProtobufFraming.read(ByteArrayInputStream(output.toByteArray())))
    }

    @Test
    fun roundTripsBoundedSemanticQueryRequestAndResponse() {
        val request = Worker.Envelope.newBuilder()
            .setProtocolVersion(WORKER_PROTOCOL_VERSION)
            .setRequestId("semantic-1")
            .setSemanticQueryRequest(
                Worker.SemanticQueryRequest.newBuilder()
                    .setCapabilityName("hierarchy.direct")
                    .setCapabilityVersion(1)
                    .addArguments(
                        Worker.SemanticQueryArgument.newBuilder()
                            .setName("supertype")
                            .setSymbolId("jvm:type:fixture.Api"),
                    )
                    .addCandidateSourceUnitIds("fixture:Impl.kt")
                    .setBudget(
                        Worker.SemanticQueryBudget.newBuilder()
                            .setMaxCandidates(10)
                            .setMaxNodes(100)
                            .setMaxBytes(4096)
                            .setDeadlineMillis(1000),
                    ),
            )
            .build()
        val response = dispatch(request)
        assertEquals("unsupported", response.semanticQueryResponse.state)
        listOf(request, response).forEach { expected ->
            val output = ByteArrayOutputStream()
            ProtobufFraming.write(output, expected)
            assertEquals(expected, ProtobufFraming.read(ByteArrayInputStream(output.toByteArray())))
        }
    }

    @Test
    fun rejectsTruncatedFramePayload() {
        assertFailsWith<java.io.EOFException> {
            ProtobufFraming.read(ByteArrayInputStream(byteArrayOf(2, 0x08)))
        }
    }
}
