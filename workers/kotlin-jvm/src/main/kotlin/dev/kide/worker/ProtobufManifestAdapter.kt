package dev.kide.worker

import kide.worker.v1.Worker
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

/** Typed bridge for portable manifests and AnalyzeBatch source snapshots. */
internal object ProtobufManifestAdapter {
    private fun provenance(value: JsonObject): Worker.Provenance = Worker.Provenance.newBuilder()
        .setBackend(value["backend"]!!.jsonPrimitive.content)
        .setBackendVersion(value["backend_version"]!!.jsonPrimitive.content)
        .setProtocolVersion(value["protocol_version"]!!.jsonPrimitive.int)
        .setAnalysisOptionsFingerprint(value["analysis_options"]!!.jsonPrimitive.content)
        .build()

    private fun json(value: Worker.Provenance): JsonObject = buildJsonObject {
        put("backend", value.backend); put("backend_version", value.backendVersion)
        put("protocol_version", value.protocolVersion); put("analysis_options", value.analysisOptionsFingerprint)
    }

    fun sourceUnit(value: JsonObject): Worker.SourceUnit = Worker.SourceUnit.newBuilder()
        .setId(value["id"]!!.jsonPrimitive.content)
        .setComponent(value["component"]!!.jsonPrimitive.content)
        .setPath(value["path"]!!.jsonPrimitive.content)
        .setLanguage(value["language"]!!.jsonPrimitive.content)
        .setOrigin(value["origin"]!!.jsonPrimitive.content)
        .setContent(value["content"]!!.jsonPrimitive.content)
        .setContext(value["context"]!!.jsonPrimitive.content)
        .build()

    fun json(value: Worker.SourceUnit): JsonObject = buildJsonObject {
        put("id", value.id); put("component", value.component); put("path", value.path)
        put("language", value.language); put("origin", value.origin); put("content", value.content); put("context", value.context)
    }

    fun analyzeBatchRequest(value: JsonObject): Worker.AnalyzeBatchRequest = Worker.AnalyzeBatchRequest.newBuilder()
        .setWorkspace(value["workspace"]!!.jsonPrimitive.content)
        .setProjectFingerprint(value["project_fingerprint"]!!.jsonPrimitive.content)
        .addAllRequestedFacts(value["requested_facts"]!!.jsonArray.map { it.jsonPrimitive.content })
        .addAllSourceUnits(value["source_units"]!!.jsonArray.map { sourceUnit(it.jsonObject) })
        .build()

    fun json(value: Worker.AnalyzeBatchRequest): JsonObject = buildJsonObject {
        put("workspace", value.workspace); put("project_fingerprint", value.projectFingerprint)
        put("requested_facts", buildJsonArray { value.requestedFactsList.forEach { add(JsonPrimitive(it)) } })
        put("source_units", buildJsonArray { value.sourceUnitsList.forEach { add(json(it)) } })
    }

    fun manifest(value: JsonObject): Worker.ProjectManifest = Worker.ProjectManifest.newBuilder()
        .setWorkspace(value["workspace"]!!.jsonPrimitive.content)
        .setRoot(value["root"]!!.jsonPrimitive.content)
        .addAllComponents(value["components"]!!.jsonArray.map { element ->
            val component = element.jsonObject
            Worker.Component.newBuilder()
                .setId(component["id"]!!.jsonPrimitive.content)
                .setName(component["name"]!!.jsonPrimitive.content)
                .setBuildSystem(component["build_system"]!!.jsonPrimitive.content)
                .setRoot(component["root"]!!.jsonPrimitive.content)
                .addAllLanguages(component["languages"]!!.jsonArray.map { it.jsonPrimitive.content })
                .setConfigurationFingerprint(component["configuration"]!!.jsonPrimitive.content)
                .addAllSourceSets(component["source_sets"]!!.jsonArray.map { sourceSet ->
                    val set = sourceSet.jsonObject
                    Worker.SourceSet.newBuilder().setName(set["name"]!!.jsonPrimitive.content)
                        .addAllSourceRoots(set["source_roots"]!!.jsonArray.map { it.jsonPrimitive.content })
                        .addAllGeneratedRoots(set["generated_roots"]!!.jsonArray.map { it.jsonPrimitive.content })
                        .setTest(set["test"]!!.jsonPrimitive.boolean).build()
                })
                .addAllClasspathFingerprints(component["classpath"]!!.jsonArray.map { it.jsonPrimitive.content })
                .apply {
                    component["toolchain"]?.takeUnless { it is JsonNull }?.jsonObject?.let { toolchain ->
                        setToolchain(Worker.Toolchain.newBuilder()
                            .setJvmVersion(toolchain["jvm_version"]!!.jsonPrimitive.content)
                            .setBuildToolVersion(toolchain["build_tool_version"]!!.jsonPrimitive.content)
                            .apply { toolchain["kotlin_version"]?.jsonPrimitive?.contentOrNull?.let(::setKotlinVersion) })
                    }
                    component["compiler_configuration"]?.jsonPrimitive?.contentOrNull
                        ?.let(::setCompilerConfigurationFingerprint)
                }.build()
        })
        .addAllDependencies(value["dependencies"]!!.jsonArray.map { element ->
            val edge = element.jsonObject
            Worker.DependencyEdge.newBuilder()
                .setFromComponentId(edge["from"]!!.jsonPrimitive.content)
                .setScope(edge["scope"]!!.jsonPrimitive.content)
                .apply {
                    val target = edge["target"]?.jsonObject ?: edge
                    when (target["target_kind"]!!.jsonPrimitive.content) {
                        "component" -> setComponentId(target["component"]!!.jsonPrimitive.content)
                        "artifact" -> setArtifactFingerprint(target["content"]!!.jsonPrimitive.content)
                        else -> error("unsupported dependency target kind")
                    }
                }.build()
        })
        .setFingerprint(value["fingerprint"]!!.jsonPrimitive.content)
        .setProvenance(provenance(value["provenance"]!!.jsonObject))
        .build()

    fun json(value: Worker.ProjectManifest): JsonObject = buildJsonObject {
        put("workspace", value.workspace); put("root", value.root)
        put("components", buildJsonArray {
            value.componentsList.forEach { component -> add(buildJsonObject {
                put("id", component.id); put("name", component.name)
                put("build_system", component.buildSystem); put("root", component.root)
                put("languages", buildJsonArray { component.languagesList.forEach { add(JsonPrimitive(it)) } })
                put("configuration", component.configurationFingerprint)
                put("source_sets", buildJsonArray { component.sourceSetsList.forEach { set -> add(buildJsonObject {
                    put("name", set.name)
                    put("source_roots", buildJsonArray { set.sourceRootsList.forEach { add(JsonPrimitive(it)) } })
                    put("generated_roots", buildJsonArray { set.generatedRootsList.forEach { add(JsonPrimitive(it)) } })
                    put("test", set.test)
                }) } })
                put("classpath", buildJsonArray { component.classpathFingerprintsList.forEach { add(JsonPrimitive(it)) } })
                put("toolchain", if (component.hasToolchain()) buildJsonObject {
                    put("jvm_version", component.toolchain.jvmVersion)
                    put("build_tool_version", component.toolchain.buildToolVersion)
                    put("kotlin_version", if (component.toolchain.hasKotlinVersion()) JsonPrimitive(component.toolchain.kotlinVersion) else JsonNull)
                } else JsonNull)
                put("compiler_configuration", if (component.hasCompilerConfigurationFingerprint()) JsonPrimitive(component.compilerConfigurationFingerprint) else JsonNull)
            }) }
        })
        put("dependencies", buildJsonArray {
            value.dependenciesList.forEach { edge -> add(buildJsonObject {
                put("from", edge.fromComponentId); put("scope", edge.scope)
                when (edge.targetCase) {
                    Worker.DependencyEdge.TargetCase.COMPONENT_ID -> { put("target_kind", "component"); put("component", edge.componentId) }
                    Worker.DependencyEdge.TargetCase.ARTIFACT_FINGERPRINT -> { put("target_kind", "artifact"); put("content", edge.artifactFingerprint) }
                    else -> error("dependency target is required")
                }
            }) }
        })
        put("fingerprint", value.fingerprint); put("provenance", json(value.provenance))
    }
}
