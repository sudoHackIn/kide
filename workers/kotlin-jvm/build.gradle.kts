plugins {
    id("com.google.protobuf") version "0.9.5"
    kotlin("jvm") version "2.4.10"
    kotlin("plugin.serialization") version "2.4.10"
    application
}

group = "dev.kide"
version = "0.1.0"

repositories {
    mavenCentral()
    maven(url = "https://repo.gradle.org/gradle/libs-releases")
}

dependencies {
    implementation("org.gradle:gradle-tooling-api:9.4.1")
    implementation("org.apache.maven:maven-embedder:3.9.9")
    implementation("org.apache.maven:maven-model:3.9.9")
    implementation("org.apache.maven.resolver:maven-resolver-connector-basic:1.9.22")
    implementation("org.apache.maven.resolver:maven-resolver-transport-http:1.9.22")
    implementation("org.ow2.asm:asm:9.8")
    implementation("org.jetbrains.kotlin:kotlin-compiler-embeddable:2.4.10")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.11.0")
    implementation("com.google.protobuf:protobuf-java:4.32.0")
    implementation("org.slf4j:slf4j-api:2.0.17")
    runtimeOnly("org.slf4j:slf4j-simple:2.0.17")
    testImplementation(kotlin("test-junit5"))
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

protobuf {
    protoc { artifact = "com.google.protobuf:protoc:4.32.0" }
}

sourceSets {
    main {
        proto.srcDir("../../protocol")
    }
}

sourceSets {
    test {
        resources.srcDir("../../protocol/fixtures")
    }
}

application {
    mainClass = "dev.kide.worker.MainKt"
}

tasks.named<JavaExec>("run") {
    standardInput = System.`in`
}

java {
    toolchain {
        languageVersion = JavaLanguageVersion.of(21)
    }
}

tasks.withType<Test>().configureEach {
    useJUnitPlatform()
    dependsOn(tasks.jar)
}
