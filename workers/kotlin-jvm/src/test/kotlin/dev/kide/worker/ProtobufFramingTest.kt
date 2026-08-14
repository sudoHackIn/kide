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
    fun rejectsTruncatedFramePayload() {
        assertFailsWith<java.io.EOFException> {
            ProtobufFraming.read(ByteArrayInputStream(byteArrayOf(2, 0x08)))
        }
    }
}
