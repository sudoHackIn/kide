package dev.kide.worker

import kotlin.test.Test
import kotlin.test.assertEquals

class HandshakeTest {
    @Test
    fun handshakeIsMachineReadableAndVersioned() {
        assertEquals(
            """{"worker":"kide-kotlin-jvm","version":"0.1.0","protocol_version":1,"capabilities":["handshake"]}""",
            handshakeJson(),
        )
    }
}
