plugins { kotlin("jvm") }

kotlin { jvmToolchain(21) }

dependencies {
    api(kotlin("stdlib"))
    implementation("com.fasterxml.jackson.core:jackson-annotations:2.21")
}
