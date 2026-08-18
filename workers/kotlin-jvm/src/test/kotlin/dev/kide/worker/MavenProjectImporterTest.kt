package dev.kide.worker

import java.nio.file.Files
import java.nio.file.Path
import kotlin.io.path.createDirectories
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNotEquals
import kotlin.test.assertTrue
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

class MavenProjectImporterTest {
    @Test
    fun importsMultiModuleReactorDeterministically() {
        val root = fixtureProject()
        val environment = mavenEnvironment(root)

        val first = MavenProjectImporter.import(root, environment).jsonObject
        val second = MavenProjectImporter.import(root, environment).jsonObject

        assertEquals(first, second)
        val components = first["components"]!!.jsonArray.map { it.jsonObject }
        assertEquals(listOf(".", "api", "app"), components.map { it["root"]!!.jsonPrimitive.content })
        assertEquals(
            listOf(
                "maven:root:main",
                "maven:api:main",
                "maven:app:main",
            ),
            components.map { it["id"]!!.jsonPrimitive.content },
        )
        assertTrue(components.all { it["build_system"]!!.jsonPrimitive.content == "maven" })

        val api = components.single { it["name"]!!.jsonPrimitive.content == "api" }
        assertEquals(listOf("java"), api["languages"]!!.jsonArray.map { it.jsonPrimitive.content })
        assertEquals(listOf("main", "test"), api["source_sets"]!!.jsonArray.map { it.jsonObject["name"]!!.jsonPrimitive.content })

        val app = components.single { it["name"]!!.jsonPrimitive.content == "app" }
        assertEquals(listOf("kotlin"), app["languages"]!!.jsonArray.map { it.jsonPrimitive.content })
        assertEquals(
            listOf("app/src/analysis/kotlin"),
            app["source_sets"]!!.jsonArray
                .single { it.jsonObject["name"]!!.jsonPrimitive.content == "main" }
                .jsonObject["source_roots"]!!.jsonArray.map { it.jsonPrimitive.content },
        )
        assertEquals(
            listOf("app/src/spec/kotlin"),
            app["source_sets"]!!.jsonArray
                .single { it.jsonObject["name"]!!.jsonPrimitive.content == "test" }
                .jsonObject["source_roots"]!!.jsonArray.map { it.jsonPrimitive.content },
        )
        assertEquals("maven-model-3.9.9", app["toolchain"]!!.jsonObject["build_tool_version"]!!.jsonPrimitive.content)
        assertTrue(app["classpath"]!!.jsonArray.isEmpty())
    }

    @Test
    fun pom_change_invalidates_manifest_fingerprint() {
        val root = fixtureProject()
        val environment = mavenEnvironment(root)
        val before = MavenProjectImporter.import(root, environment).jsonObject["fingerprint"]!!.jsonPrimitive.content

        Files.writeString(root.resolve("api/pom.xml"), """
            <project><modelVersion>4.0.0</modelVersion><parent><groupId>fixture</groupId><artifactId>reactor</artifactId><version>1</version></parent><artifactId>api</artifactId><version>2</version></project>
        """.trimIndent())

        val after = MavenProjectImporter.import(root, environment).jsonObject["fingerprint"]!!.jsonPrimitive.content
        assertNotEquals(before, after)
    }

    @Test
    fun rejectsSourceRootSymlinkEscapingWorkspace() {
        val root = Files.createTempDirectory("kide-maven-import-")
        val environment = mavenEnvironment(root)
        val external = Files.createTempDirectory("kide-maven-external-")
        write(root.resolve("pom.xml"), """
            <project><modelVersion>4.0.0</modelVersion><groupId>fixture</groupId><artifactId>app</artifactId><version>1</version><build><sourceDirectory>linked</sourceDirectory></build></project>
        """.trimIndent())
        try {
            Files.createSymbolicLink(root.resolve("linked"), external)
        } catch (error: UnsupportedOperationException) {
            return
        }

        val failure = assertFailsWith<IllegalArgumentException> { MavenProjectImporter.import(root, environment) }
        assertTrue(failure.message!!.contains("escapes workspace root"))
    }

    @Test
    fun selectsConfiguredMavenBeforeWrapperAndPath() {
        val root = Files.createTempDirectory("kide-maven-selection-")
        val configured = fakeMaven(root.resolve("configured-maven"))

        assertEquals(
            configured,
            MavenProjectImporter.selectMaven(root, mapOf("KIDE_MAVEN_HOME" to configured.parent.parent.toString(), "PATH" to "")),
        )
    }

    @Test
    fun realMavenResolvesSpringCrudFixtureWhenConfigured() {
        val mavenHome = System.getenv("KIDE_MAVEN_HOME") ?: return
        val root = Path.of("../..", "fixtures/maven-spring-boot-crud").toAbsolutePath().normalize()
        val environment = System.getenv() + mapOf("KIDE_MAVEN_HOME" to mavenHome)

        val manifest = MavenProjectImporter.import(root, environment).jsonObject
        val artifacts = MavenProjectImporter.resolvedArtifacts(root, environment)
        val springWeb = artifacts.first { it.path.fileName.toString().startsWith("spring-web-") }
        val descriptor = JvmBytecodeExtractor.descriptor(springWeb.path, springWeb.component, springWeb.context).jsonObject
        val materialized = ArtifactMaterializer.stage(
            springWeb.path,
            springWeb.component,
            springWeb.context,
            Files.createTempDirectory("kide-maven-artifact-stage-"),
        )
        val contexts = MavenProjectImporter.javaCompilationContexts(root)
        val domain = contexts.single { context -> context.sourceFiles.any { it.fileName.toString() == "Book.java" } }
        val app = contexts.single { context -> context.sourceFiles.any { it.fileName.toString() == "BookController.java" } }
        fun sourceUnit(context: GradleProjectImporter.JavaCompilationContext, path: Path) = buildJsonObject {
            val relative = root.relativize(path).toString()
            put("id", "${context.component}:$relative")
            put("component", context.component)
            put("path", relative)
            put("language", "java")
            put("context", "sha256:maven-test")
        }
        val domainSource = domain.sourceFiles.single { it.fileName.toString() == "Book.java" }
        val controllerSource = app.sourceFiles.single { it.fileName.toString() == "BookController.java" }
        val snapshots = JavaSemanticExtractor.analyze(
            listOf(
                sourceUnit(domain, domainSource),
                sourceUnit(app, controllerSource),
            ),
            root,
        ).map { it.jsonObject }
        val book = snapshots.flatMap { it["symbols"]!!.jsonArray }.single { symbol ->
            symbol.jsonObject["qualified_name"]!!.jsonPrimitive.content == "dev.kide.fixture.domain.Book"
        }.jsonObject["id"]!!.jsonPrimitive.content
        val controller = snapshots.single { snapshot ->
            snapshot["source_unit"]!!.jsonObject["path"]!!.jsonPrimitive.content.endsWith("BookController.java")
        }
        val incrementalController = JavaSemanticExtractor.analyze(listOf(sourceUnit(app, controllerSource)), root)
            .single().jsonObject

        assertEquals(3, manifest["components"]!!.jsonArray.size)
        assertTrue(descriptor["source_unit"]!!.jsonObject["id"]!!.jsonPrimitive.content.isNotBlank())
        assertTrue(materialized.byteLength > 0)
        assertTrue(manifest["components"]!!.jsonArray.any { it.jsonObject["classpath"]!!.jsonArray.isNotEmpty() })
        assertTrue(
            controller["references"]!!.jsonArray.any { reference -> reference.jsonObject["target"]!!.jsonPrimitive.content == book },
            "Book reference was not preserved: ${controller["references"]}",
        )
        assertTrue(
            incrementalController["references"]!!.jsonArray.any { reference -> reference.jsonObject["target"]!!.jsonPrimitive.content == book },
            "Incremental Book reference was not preserved: ${incrementalController["references"]}",
        )
    }

    private fun fixtureProject(): Path {
        val root = Files.createTempDirectory("kide-maven-import-")
        write(root.resolve("pom.xml"), """
            <project><modelVersion>4.0.0</modelVersion><groupId>fixture</groupId><artifactId>reactor</artifactId><version>1</version><packaging>pom</packaging><modules><module>app</module><module>api</module></modules></project>
        """.trimIndent())
        write(root.resolve("api/pom.xml"), """
            <project><modelVersion>4.0.0</modelVersion><parent><groupId>fixture</groupId><artifactId>reactor</artifactId><version>1</version></parent><artifactId>api</artifactId></project>
        """.trimIndent())
        write(root.resolve("app/pom.xml"), """
            <project><modelVersion>4.0.0</modelVersion><parent><groupId>fixture</groupId><artifactId>reactor</artifactId><version>1</version></parent><artifactId>app</artifactId><build><plugins><plugin><groupId>org.jetbrains.kotlin</groupId><artifactId>kotlin-maven-plugin</artifactId><configuration><sourceDirs><source>src/analysis/kotlin</source></sourceDirs></configuration><executions><execution><id>tests</id><goals><goal>test-compile</goal></goals><configuration><sourceDirs><source>src/spec/kotlin</source></sourceDirs></configuration></execution></executions></plugin></plugins></build></project>
        """.trimIndent())
        write(root.resolve("api/src/main/java/Api.java"), "package fixture; class Api {}")
        write(root.resolve("api/src/test/java/ApiTest.java"), "package fixture; class ApiTest {}")
        write(root.resolve("app/src/analysis/kotlin/App.kt"), "package fixture\nclass App")
        write(root.resolve("app/src/spec/kotlin/AppTest.kt"), "package fixture\nclass AppTest")
        return root
    }

    private fun write(path: Path, contents: String) {
        path.parent.createDirectories()
        Files.writeString(path, "$contents\n")
    }

    private fun mavenEnvironment(root: Path): Map<String, String> {
        val executable = fakeMaven(root.resolve("fake-maven"))
        return mapOf("KIDE_MAVEN_HOME" to executable.parent.parent.toString(), "PATH" to "")
    }

    /** A controlled Maven stand-in: it writes the requested input POM as the effective POM. */
    private fun fakeMaven(home: Path): Path {
        val executable = home.resolve("bin/mvn")
        write(executable, """
            #!/bin/sh
            pom=""
            output=""
            while [ "${'$'}#" -gt 0 ]; do
              case "${'$'}1" in
                -f) pom="${'$'}2"; shift 2 ;;
                -Doutput) output="${'$'}2"; shift 2 ;;
                -Doutput=*) output="${'$'}{1#-Doutput=}"; shift ;;
                -Dmdep.outputFile=*) output="${'$'}{1#-Dmdep.outputFile=}"; printf '%s' "${'$'}KIDE_TEST_ARTIFACT" > "${'$'}output"; exit 0 ;;
                *) shift ;;
              esac
            done
            cp "${'$'}pom" "${'$'}output"
        """.trimIndent())
        require(executable.toFile().setExecutable(true)) { "cannot make fake Maven executable" }
        return executable
    }
}
