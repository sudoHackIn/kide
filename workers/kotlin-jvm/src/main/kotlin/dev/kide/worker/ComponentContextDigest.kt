package dev.kide.worker

import java.security.MessageDigest

/**
 * Backend-neutral owner identity for one resolved compilation context.
 *
 * Importers adapt their build-tool models into sorted, opaque strings. Core
 * receives only the resulting fingerprint, so it never needs to understand
 * Maven scopes, Gradle configurations, or worker-local artifact paths.
 */
internal object ComponentContextDigest {
    fun fingerprint(
        component: String,
        sourceSets: List<String>,
        artifacts: List<String>,
        dependencyEdges: List<String>,
        toolchain: List<String>,
    ): String {
        val digest = MessageDigest.getInstance("SHA-256")
        update(digest, "schema", "component-context-v1")
        update(digest, "component", component)
        sourceSets.sorted().forEach { update(digest, "source-set", it) }
        artifacts.sorted().forEach { update(digest, "artifact", it) }
        dependencyEdges.sorted().forEach { update(digest, "dependency", it) }
        (toolchain + listOf(
            "worker=$WORKER_NAME@$WORKER_VERSION",
            "worker-protocol=$WORKER_PROTOCOL_VERSION",
        )).sorted().forEach { update(digest, "toolchain", it) }
        return "sha256:${digest.digest().joinToString("") { byte -> "%02x".format(byte) }}"
    }

    private fun update(digest: MessageDigest, kind: String, value: String) {
        digest.update(kind.encodeToByteArray())
        digest.update(0)
        digest.update(value.encodeToByteArray())
        digest.update(0)
    }
}
