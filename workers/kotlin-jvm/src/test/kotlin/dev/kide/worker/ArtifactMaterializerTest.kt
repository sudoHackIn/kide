package dev.kide.worker

import java.nio.file.Files
import javax.tools.ToolProvider
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class ArtifactMaterializerTest {
    @Test
    fun stagesAnFsyncedJvmBlobWithControlPlaneMetadata() {
        val classes = Files.createTempDirectory("kide-materialize-classes-")
        val source = classes.resolve("fixture/Widget.java")
        Files.createDirectories(source.parent)
        Files.writeString(source, "package fixture; public class Widget { public String name; }")
        assertEquals(0, ToolProvider.getSystemJavaCompiler().run(null, null, null, "-d", classes.toString(), source.toString()))
        val staging = Files.createTempDirectory("kide-materialize-staging-")

        val result = ArtifactMaterializer.stage(classes, "gradle:app:main", "sha256:context", staging)
        val bytes = Files.readAllBytes(staging.resolve(result.stagedFilename))

        assertTrue(bytes.copyOfRange(0, 8).decodeToString() == "KIDEJVM1")
        assertEquals(bytes.size.toLong(), result.byteLength)
        assertEquals(32, result.sha256.size())
        assertEquals(JvmArtifactBlobLayout.VERSION, result.blobFormatVersion)
    }
}
