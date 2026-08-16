package dev.kide.worker

import java.nio.file.Files
import java.nio.file.Path
import kotlin.io.path.createDirectories
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotEquals
import kotlin.test.assertTrue
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

class GradleProjectImporterTest {
    @Test
    fun importsMultiModuleKotlinJvmModelDeterministically() {
        val root = fixtureProject()

        val first = GradleProjectImporter.import(root).jsonObject
        val second = GradleProjectImporter.import(root).jsonObject

        assertEquals(first, second)
        val components = first["components"]!!.jsonArray
        assertEquals(3, components.size)
        val sourceComponents = components.filter { component ->
            component.jsonObject["source_sets"]!!.jsonArray.isNotEmpty()
        }
        assertEquals(2, sourceComponents.size)
        assertTrue(sourceComponents.all { component ->
            component.jsonObject["source_sets"]!!.jsonArray.any { sourceSet -> sourceSet.jsonObject["name"]!!.jsonPrimitive.content == "main" }
        })
        assertTrue(sourceComponents.all { component ->
            component.jsonObject["source_sets"]!!.jsonArray.any { sourceSet -> sourceSet.jsonObject["name"]!!.jsonPrimitive.content == "test" }
        })
        assertTrue(first["dependencies"]!!.jsonArray.any { dependency ->
            dependency.jsonObject["target"]!!.jsonObject["target_kind"]!!.jsonPrimitive.content == "component"
        })
        assertTrue(first["dependencies"]!!.jsonArray.any { dependency ->
            dependency.jsonObject["target"]!!.jsonObject["target_kind"]!!.jsonPrimitive.content == "artifact"
        })
        assertTrue(components.any { component -> component.jsonObject["classpath"]!!.jsonArray.isNotEmpty() })
        assertEquals("gradle", components.first().jsonObject["build_system"]!!.jsonPrimitive.content)
    }

    @Test
    fun relevant_build_configuration_changes_manifest_fingerprint() {
        val root = fixtureProject()
        val before = GradleProjectImporter.import(root).jsonObject["fingerprint"]!!.jsonPrimitive.content

        write(root.resolve("gradle.properties"), "kide.test.flag=changed\n")
        val after = GradleProjectImporter.import(root).jsonObject["fingerprint"]!!.jsonPrimitive.content

        assertNotEquals(before, after)
    }

    @Test
    fun resolves_explicit_installation_before_local_wrapper_cache() {
        val root = fixtureProject()
        val explicit = Files.createTempDirectory("kide-gradle-explicit-")
        val cache = Files.createTempDirectory("kide-gradle-cache-")
        gradleInstallation(explicit)
        gradleInstallation(cache.resolve("wrapper/dists/gradle-8.14-bin/hash/gradle-8.14"))
        write(root.resolve("gradle/wrapper/gradle-wrapper.properties"), "distributionUrl=https\\://services.gradle.org/distributions/gradle-8.14-bin.zip")

        assertEquals(
            explicit,
            GradleProjectImporter.resolveGradleInstallation(
                root,
                mapOf("KIDE_GRADLE_INSTALLATION" to explicit.toString(), "GRADLE_USER_HOME" to cache.toString()),
            ),
        )
    }

    @Test
    fun resolves_installed_wrapper_distribution_from_gradle_user_home() {
        val root = fixtureProject()
        val cache = Files.createTempDirectory("kide-gradle-cache-")
        val installation = cache.resolve("wrapper/dists/gradle-8.14-bin/hash/gradle-8.14")
        gradleInstallation(installation)
        write(root.resolve("gradle/wrapper/gradle-wrapper.properties"), "distributionUrl=https\\://services.gradle.org/distributions/gradle-8.14-bin.zip")

        assertEquals(
            installation,
            GradleProjectImporter.resolveGradleInstallation(root, mapOf("GRADLE_USER_HOME" to cache.toString())),
        )
    }

    private fun fixtureProject(): Path {
        val root = Files.createTempDirectory("kide-gradle-import-")
        write(root.resolve("settings.gradle.kts"), """
            pluginManagement { repositories { gradlePluginPortal(); mavenCentral() } }
            dependencyResolutionManagement { repositories { mavenCentral() } }
            rootProject.name = "kide-import-fixture"
            include(":api", ":app")
        """.trimIndent())
        write(root.resolve("api/build.gradle.kts"), """
            plugins { kotlin("jvm") version "2.4.10" }
        """.trimIndent())
        write(root.resolve("app/build.gradle.kts"), """
            plugins { kotlin("jvm") version "2.4.10" }
            dependencies {
                implementation(project(":api"))
                implementation("org.jetbrains.kotlin:kotlin-stdlib:2.4.10")
            }
        """.trimIndent())
        write(root.resolve("api/src/main/kotlin/Api.kt"), "package fixture\nclass Api\n")
        write(root.resolve("api/src/test/kotlin/ApiTest.kt"), "package fixture\nclass ApiTest\n")
        write(root.resolve("app/src/main/kotlin/App.kt"), "package fixture\nclass App\n")
        write(root.resolve("app/src/test/kotlin/AppTest.kt"), "package fixture\nclass AppTest\n")
        return root
    }

    private fun write(path: Path, contents: String) {
        path.parent.createDirectories()
        Files.writeString(path, "$contents\n")
    }

    private fun gradleInstallation(path: Path) {
        path.resolve("bin").createDirectories()
        Files.writeString(path.resolve("bin/gradle"), "#!/bin/sh\n")
    }
}
