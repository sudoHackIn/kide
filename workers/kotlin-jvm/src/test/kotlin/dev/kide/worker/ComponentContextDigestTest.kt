package dev.kide.worker

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotEquals

class ComponentContextDigestTest {
    private fun fingerprint(
        artifacts: List<String> = listOf("sha256:jar-a"),
        edges: List<String> = listOf("compile:org.example:api:jar:1"),
        toolchain: List<String> = listOf("build-tool=test", "jvm=21"),
    ) = ComponentContextDigest.fingerprint(
        component = "fixture:main",
        sourceSets = listOf("main:false:src/main/java:java"),
        artifacts = artifacts,
        dependencyEdges = edges,
        toolchain = toolchain,
    )

    @Test
    fun `normalizes ordering but distinguishes every context owner input`() {
        val baseline = fingerprint(
            artifacts = listOf("sha256:jar-b", "sha256:jar-a"),
            edges = listOf("runtime:org.example:runtime:jar:1", "compile:org.example:api:jar:1"),
            toolchain = listOf("jvm=21", "build-tool=test"),
        )
        assertEquals(
            baseline,
            fingerprint(
                artifacts = listOf("sha256:jar-a", "sha256:jar-b"),
                edges = listOf("compile:org.example:api:jar:1", "runtime:org.example:runtime:jar:1"),
            ),
        )
        assertNotEquals(baseline, fingerprint(artifacts = listOf("sha256:jar-c", "sha256:jar-a")))
        assertNotEquals(baseline, fingerprint(edges = listOf("runtime:org.example:api:jar:1", "runtime:org.example:runtime:jar:1")))
        assertNotEquals(baseline, fingerprint(toolchain = listOf("build-tool=test", "jvm=22")))
    }
}
