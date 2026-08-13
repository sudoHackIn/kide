package dev.kide.worker

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.JsonElement

class ProtocolFixtureTest {
    @Test
    fun everyMvpMessageRoundTripsItsGoldenFixture() {
        fixtureNames.forEach { name ->
            val expected = protocolJson.parseToJsonElement(readFixture(name))
            val decoded = protocolJson.decodeFromString<WorkerEnvelope>(expected.toString())
            val actual: JsonElement = protocolJson.parseToJsonElement(protocolJson.encodeToString(decoded))

            assertEquals(expected, actual, "$name preserves the wire contract")
        }
    }

    private fun readFixture(name: String): String =
        checkNotNull(javaClass.classLoader.getResourceAsStream(name)) { "missing fixture $name" }
            .bufferedReader()
            .use { it.readText() }

    private companion object {
        val fixtureNames = listOf(
            "handshake-request.json",
            "handshake-response.json",
            "project-manifest-request.json",
            "project-manifest-response.json",
            "analyze-batch-request.json",
            "analysis-batch-response.json",
            "analysis-delta.json",
            "error.json",
        )
    }
}
