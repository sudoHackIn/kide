package dev.kide.worker

import kide.worker.v1.Worker
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long

/** Typed protobuf writer for a canonical analysis snapshot; no fact is packed as JSON bytes. */
internal object ProtobufAnalysisSnapshotAdapter {
    private fun provenance(value: JsonObject): Worker.Provenance = Worker.Provenance.newBuilder()
        .setBackend(value["backend"]!!.jsonPrimitive.content)
        .setBackendVersion(value["backend_version"]!!.jsonPrimitive.content)
        .setProtocolVersion(value["protocol_version"]!!.jsonPrimitive.int)
        .setAnalysisOptionsFingerprint(value["analysis_options"]!!.jsonPrimitive.content)
        .build()

    private fun range(value: JsonObject): Worker.ByteRange = Worker.ByteRange.newBuilder()
        .setStart(value["start"]!!.jsonPrimitive.long)
        .setEnd(value["end"]!!.jsonPrimitive.long)
        .build()

    private fun json(value: Worker.ByteRange): JsonObject = buildJsonObject {
        put("start", value.start)
        put("end", value.end)
    }

    private fun json(value: Worker.Provenance): JsonObject = buildJsonObject {
        put("backend", value.backend)
        put("backend_version", value.backendVersion)
        put("protocol_version", value.protocolVersion)
        put("analysis_options", value.analysisOptionsFingerprint)
    }

    private fun nullable(value: String?, present: Boolean) =
        if (present) JsonPrimitive(requireNotNull(value)) else JsonNull

    private fun symbol(value: JsonObject): Worker.SymbolDeclaration {
        val key = value["backend_key"]!!.jsonObject
        return Worker.SymbolDeclaration.newBuilder()
            .setId(value["id"]!!.jsonPrimitive.content)
            .setSourceUnitIndex(0).setProvenanceIndex(0)
            .setBackendKey(key["value"]!!.jsonPrimitive.content)
            .setBackendSchemaVersion(key["schema_version"]!!.jsonPrimitive.int)
            .setLanguage(value["language"]!!.jsonPrimitive.content)
            .setKind(value["kind"]!!.jsonPrimitive.content)
            .setName(value["name"]!!.jsonPrimitive.content)
            .setComponentId(value["component"]!!.jsonPrimitive.content)
            .setDeclaration(range(value["declaration"]!!.jsonObject["bytes"]!!.jsonObject))
            .setNameRange(range(value["name_range"]!!.jsonObject["bytes"]!!.jsonObject))
            .setFreshness(value["freshness"]!!.jsonPrimitive.content)
            .setCompleteness(value["completeness"]!!.jsonPrimitive.content)
            .apply {
                value["qualified_name"]?.jsonPrimitive?.contentOrNull?.let(::setQualifiedName)
                value["signature"]?.jsonPrimitive?.contentOrNull?.let(::setSignature)
                value["owner"]?.jsonPrimitive?.contentOrNull?.let(::setOwnerId)
                addAllModifiers(value["modifiers"]!!.jsonArray.map { it.jsonPrimitive.content })
                addAllAppliedSymbolIds(value["applied_symbols"]?.jsonArray?.map { it.jsonPrimitive.content } ?: emptyList())
            }.build()
    }

    private fun occurrence(value: JsonObject): Worker.Occurrence {
        val location = value["range"]!!.jsonObject
        return Worker.Occurrence.newBuilder()
            .setLocation(Worker.SourceLocation.newBuilder().setSourceUnitIndex(0).setRange(range(location["bytes"]!!.jsonObject)))
            .setKind(value["kind"]!!.jsonPrimitive.content)
            .setPrecision(value["precision"]!!.jsonPrimitive.content)
            .setFreshness(value["freshness"]!!.jsonPrimitive.content)
            .setCompleteness(value["completeness"]!!.jsonPrimitive.content)
            .setProvenanceIndex(0).apply {
                value["enclosing_symbol"]?.jsonPrimitive?.contentOrNull?.let(::setEnclosingSymbolId)
                value["target"]?.jsonPrimitive?.contentOrNull?.let(::setTargetSymbolId)
                value["type_id"]?.jsonPrimitive?.contentOrNull?.let(::setTypeId)
            }.build()
    }

    private fun reference(value: JsonObject, occurrenceIndex: Int): Worker.ReferenceEdge =
        Worker.ReferenceEdge.newBuilder().setSourceOccurrenceIndex(occurrenceIndex)
            .setTargetSymbolId(value["target"]!!.jsonPrimitive.content)
            .setPrecision(value["precision"]!!.jsonPrimitive.content).build()

    private fun call(value: JsonObject, occurrenceIndex: Int): Worker.CallEdge =
        Worker.CallEdge.newBuilder().setSourceOccurrenceIndex(occurrenceIndex)
            .setTargetSymbolId(value["target"]!!.jsonPrimitive.content)
            .setPrecision(value["precision"]!!.jsonPrimitive.content).apply {
                value["caller"]?.jsonPrimitive?.contentOrNull?.let(::setCallerSymbolId)
            }.build()

    private fun hierarchy(value: JsonObject): Worker.HierarchyEdge = Worker.HierarchyEdge.newBuilder()
        .setSubtypeSymbolId(value["subtype"]!!.jsonPrimitive.content)
        .setSupertypeSymbolId(value["supertype"]!!.jsonPrimitive.content)
        .setPrecision(value["precision"]!!.jsonPrimitive.content)
        .setProvenanceIndex(0).build()

    private fun application(value: JsonObject): Worker.ApplicationFact {
        val range = value["range"]!!.jsonObject["bytes"]!!.jsonObject
        return Worker.ApplicationFact.newBuilder().setId(value["id"]!!.jsonPrimitive.content)
            .setSubjectSymbolId(value["subject"]!!.jsonPrimitive.content).setTargetSymbolId(value["target"]!!.jsonPrimitive.content)
            .setRange(range(range)).setPrecision(value["precision"]!!.jsonPrimitive.content).setFreshness(value["freshness"]!!.jsonPrimitive.content)
            .setCompleteness(value["completeness"]!!.jsonPrimitive.content).setProvenanceIndex(0).apply {
                value["arguments"]!!.jsonArray.forEach { argument ->
                    val record = argument.jsonObject; val literal = record["value"]!!.jsonObject
                    addArguments(Worker.ApplicationArgument.newBuilder().setPosition(record["position"]!!.jsonPrimitive.int).apply {
                        record["name"]?.jsonPrimitive?.contentOrNull?.let(::setName)
                        when (literal["kind"]!!.jsonPrimitive.content) { "string" -> setStringValue(literal["value"]!!.jsonPrimitive.content); "string_list" -> { setStringValue(""); addAllStringListValue(literal["value"]!!.jsonArray.map { it.jsonPrimitive.content }) }; "boolean" -> setBooleanValue(literal["value"]!!.jsonPrimitive.content.toBoolean()); "integer" -> setIntegerValue(literal["value"]!!.jsonPrimitive.long) }
                    })
                }
            }.build()
    }

    private fun type(value: JsonObject): Worker.TypeRecord = Worker.TypeRecord.newBuilder()
        .setId(value["id"]!!.jsonPrimitive.content)
        .setLanguage(value["language"]!!.jsonPrimitive.content)
        .setDisplay(value["display"]!!.jsonPrimitive.content)
        .setFreshness(value["freshness"]!!.jsonPrimitive.content)
        .setCompleteness(value["completeness"]!!.jsonPrimitive.content)
        .setProvenanceIndex(0).apply {
            value["backend_key"]?.takeUnless { it is JsonNull }?.jsonObject?.let { key ->
                setBackendKey(key["value"]!!.jsonPrimitive.content)
                setBackendSchemaVersion(key["schema_version"]!!.jsonPrimitive.int)
            }
        }.build()

    private fun diagnostic(value: JsonObject): Worker.Diagnostic = Worker.Diagnostic.newBuilder()
        .setSourceUnitIndex(0).setSeverity(value["severity"]!!.jsonPrimitive.content)
        .setMessage(value["message"]!!.jsonPrimitive.content)
        .setFreshness(value["freshness"]!!.jsonPrimitive.content)
        .setCompleteness(value["completeness"]!!.jsonPrimitive.content)
        .setProvenanceIndex(0).apply {
            value["range"]?.takeUnless { it is JsonNull }?.let { setRange(range(it.jsonObject)) }
            value["code"]?.jsonPrimitive?.contentOrNull?.let(::setCode)
        }.build()

    fun snapshot(value: JsonObject): Worker.FileAnalysisSnapshot {
        val provenanceValues = mutableListOf(value["provenance"]!!.jsonObject)
        fun provenanceIndex(record: JsonObject): Int {
            val factProvenance = record["provenance"]?.takeUnless { it is JsonNull }?.jsonObject
                ?: provenanceValues.first()
            val existing = provenanceValues.indexOf(factProvenance)
            if (existing >= 0) return existing
            provenanceValues += factProvenance
            return provenanceValues.lastIndex
        }

        val canonicalOccurrences = value["occurrences"]!!.jsonArray
        val symbols = value["symbols"]!!.jsonArray.map {
            val record = it.jsonObject
            symbol(record).toBuilder().setProvenanceIndex(provenanceIndex(record)).build()
        }
        val occurrences = canonicalOccurrences.map {
            val record = it.jsonObject
            occurrence(record).toBuilder().setProvenanceIndex(provenanceIndex(record)).build()
        }
        val references = value["references"]!!.jsonArray.map { edge ->
            val record = edge.jsonObject
            val index = canonicalOccurrences.indexOfFirst { it.jsonObject == record["source"]!!.jsonObject }
            require(index >= 0) { "reference source occurrence is absent from snapshot" }
            reference(record, index)
        }
        val calls = value["calls"]!!.jsonArray.map { edge ->
            val record = edge.jsonObject
            val index = canonicalOccurrences.indexOfFirst { it.jsonObject == record["source"]!!.jsonObject }
            require(index >= 0) { "call source occurrence is absent from snapshot" }
            call(record, index)
        }
        val hierarchy = value["hierarchy"]!!.jsonArray.map {
            val record = it.jsonObject
            hierarchy(record).toBuilder().setProvenanceIndex(provenanceIndex(record)).build()
        }
        val types = value["types"]!!.jsonArray.map {
            val record = it.jsonObject
            type(record).toBuilder().setProvenanceIndex(provenanceIndex(record)).build()
        }
        val diagnostics = value["diagnostics"]!!.jsonArray.map {
            val record = it.jsonObject
            diagnostic(record).toBuilder().setProvenanceIndex(provenanceIndex(record)).build()
        }
        val applications = value["applications"]?.jsonArray?.map {
            val record = it.jsonObject
            application(record).toBuilder().setProvenanceIndex(provenanceIndex(record)).build()
        } ?: emptyList()

        return Worker.FileAnalysisSnapshot.newBuilder()
            .setSourceUnit(ProtobufManifestAdapter.sourceUnit(value["source_unit"]!!.jsonObject))
            .apply {
                value["structural_fingerprint"]?.jsonPrimitive?.contentOrNull?.let(::setStructuralFingerprint)
                value["public_api_fingerprint"]?.jsonPrimitive?.contentOrNull?.let(::setPublicApiFingerprint)
            }
            .addAllProvenances(provenanceValues.map(::provenance))
            .addAllSymbols(symbols)
            .addAllApplications(applications)
            .addAllOccurrences(occurrences)
            .addAllReferences(references)
            .addAllCalls(calls)
            .addAllHierarchy(hierarchy)
            .addAllTypes(types)
            .addAllDiagnostics(diagnostics)
            .setCompleteness(value["completeness"]!!.jsonPrimitive.content)
            .setProvenanceIndex(0)
            .build()
    }

    fun analysisBatchResponse(value: JsonObject): Worker.AnalysisBatchResponse =
        Worker.AnalysisBatchResponse.newBuilder()
            .addAllSnapshots(value["snapshots"]!!.jsonArray.map { snapshot(it.jsonObject) })
            .addAllTimings(value["timings"]?.jsonArray?.map { timing ->
                val json = timing.jsonObject
                Worker.PhaseTiming.newBuilder()
                    .setPhase(json["phase"]!!.jsonPrimitive.content)
                    .setElapsedMillis(json["elapsed_millis"]!!.jsonPrimitive.long)
                    .build()
            }.orEmpty())
            .build()

    fun json(value: Worker.AnalysisBatchResponse): JsonObject = buildJsonObject {
        put("snapshots", buildJsonArray { value.snapshotsList.forEach { add(json(it)) } })
        put("timings", buildJsonArray { value.timingsList.forEach { timing -> add(buildJsonObject {
            put("phase", timing.phase); put("elapsed_millis", timing.elapsedMillis)
        }) } })
    }

    fun delta(value: JsonObject): Worker.AnalysisDelta = Worker.AnalysisDelta.newBuilder()
        .setSourceUnitId(value["source_unit"]!!.jsonPrimitive.content)
        .apply {
            value["previous_content"]?.jsonPrimitive?.contentOrNull?.let(::setPreviousContentFingerprint)
            value["snapshot"]?.takeUnless { it is JsonNull }?.jsonObject?.let { setSnapshot(snapshot(it)) }
        }
        .build()

    fun json(value: Worker.AnalysisDelta): JsonObject = buildJsonObject {
        put("source_unit", value.sourceUnitId)
        put("previous_content", if (value.hasPreviousContentFingerprint()) JsonPrimitive(value.previousContentFingerprint) else JsonNull)
        put("snapshot", if (value.hasSnapshot()) json(value.snapshot) else JsonNull)
    }

    fun artifactAnalysisRequest(value: JsonObject): Worker.ArtifactAnalysisRequest =
        Worker.ArtifactAnalysisRequest.newBuilder()
            .setWorkspaceRoot(value["workspace_root"]!!.jsonPrimitive.content)
            .setMaxArtifacts(value["max_artifacts"]!!.jsonPrimitive.int)
            .apply { value["cursor"]?.jsonPrimitive?.contentOrNull?.let(::setCursor) }
            .build()

    fun json(value: Worker.ArtifactAnalysisRequest): JsonObject = buildJsonObject {
        require(value.maxArtifacts > 0) { "max_artifacts must be positive" }
        put("workspace_root", value.workspaceRoot)
        put("max_artifacts", value.maxArtifacts)
        put("cursor", if (value.hasCursor()) JsonPrimitive(value.cursor) else JsonNull)
    }

    fun artifactAnalysisResponse(value: JsonObject): Worker.ArtifactAnalysisResponse =
        Worker.ArtifactAnalysisResponse.newBuilder()
            .addAllSnapshots(value["snapshots"]!!.jsonArray.map { snapshot(it.jsonObject) })
            .apply { value["next_cursor"]?.jsonPrimitive?.contentOrNull?.let(::setNextCursor) }
            .build()

    fun json(value: Worker.ArtifactAnalysisResponse): JsonObject = buildJsonObject {
        put("snapshots", buildJsonArray { value.snapshotsList.forEach { add(json(it)) } })
        put("next_cursor", if (value.hasNextCursor()) JsonPrimitive(value.nextCursor) else JsonNull)
    }

    fun json(value: Worker.FileAnalysisSnapshot): JsonObject {
        fun factProvenance(index: Int): Worker.Provenance {
            require(index in value.provenancesList.indices) { "provenance index out of range: $index" }
            return value.provenancesList[index]
        }

        val occurrences = buildJsonArray {
            value.occurrencesList.forEach { occurrence ->
                require(occurrence.hasLocation() && occurrence.location.hasRange()) {
                    "occurrence location/range is required"
                }
                add(buildJsonObject {
                    val location = occurrence.location
                    put("range", buildJsonObject {
                        put("source_unit", value.sourceUnit.id)
                        put("bytes", json(location.range))
                    })
                    put("kind", occurrence.kind)
                    put("enclosing_symbol", nullable(occurrence.enclosingSymbolId, occurrence.hasEnclosingSymbolId()))
                    put("target", nullable(occurrence.targetSymbolId, occurrence.hasTargetSymbolId()))
                    put("type_id", nullable(occurrence.typeId, occurrence.hasTypeId()))
                    put("precision", occurrence.precision)
                    put("freshness", occurrence.freshness)
                    put("completeness", occurrence.completeness)
                    put("provenance", json(factProvenance(occurrence.provenanceIndex)))
                })
            }
        }

        return buildJsonObject {
            put("source_unit", ProtobufManifestAdapter.json(value.sourceUnit))
            put("structural_fingerprint", nullable(value.structuralFingerprint, value.hasStructuralFingerprint()))
            put("public_api_fingerprint", nullable(value.publicApiFingerprint, value.hasPublicApiFingerprint()))
            put("symbols", buildJsonArray {
                value.symbolsList.forEach { symbol ->
                    require(symbol.hasDeclaration() && symbol.hasNameRange()) {
                        "symbol declaration/name range is required"
                    }
                    val factProvenance = factProvenance(symbol.provenanceIndex)
                    add(buildJsonObject {
                        put("id", symbol.id)
                        put("backend_key", buildJsonObject {
                            put("backend", factProvenance.backend)
                            put("schema_version", symbol.backendSchemaVersion)
                            put("value", symbol.backendKey)
                        })
                        put("language", symbol.language)
                        put("kind", symbol.kind)
                        put("name", symbol.name)
                        put("qualified_name", nullable(symbol.qualifiedName, symbol.hasQualifiedName()))
                        put("signature", nullable(symbol.signature, symbol.hasSignature()))
                        put("component", symbol.componentId)
                        put("declaration", buildJsonObject {
                            put("source_unit", value.sourceUnit.id)
                            put("bytes", json(symbol.declaration))
                        })
                        put("name_range", buildJsonObject {
                            put("source_unit", value.sourceUnit.id)
                            put("bytes", json(symbol.nameRange))
                        })
                        put("owner", nullable(symbol.ownerId, symbol.hasOwnerId()))
                        put("modifiers", buildJsonArray { symbol.modifiersList.forEach { add(JsonPrimitive(it)) } })
                        put("applied_symbols", buildJsonArray { symbol.appliedSymbolIdsList.forEach { add(JsonPrimitive(it)) } })
                        put("freshness", symbol.freshness)
                        put("completeness", symbol.completeness)
                        put("provenance", json(factProvenance))
                    })
                }
            })
            put("applications", buildJsonArray {
                value.applicationsList.forEach { application ->
                    require(application.hasRange()) { "application range is required" }
                    val provenance = factProvenance(application.provenanceIndex)
                    add(buildJsonObject {
                        put("id", application.id); put("subject", application.subjectSymbolId); put("target", application.targetSymbolId)
                        put("range", buildJsonObject { put("source_unit", value.sourceUnit.id); put("bytes", json(application.range)) })
                        put("arguments", buildJsonArray {
                            application.argumentsList.forEach { argument ->
                                add(buildJsonObject {
                                    put("name", nullable(argument.name, argument.hasName()))
                                    put("position", argument.position)
                                    put("value", buildJsonObject {
                                        when {
                                            argument.stringListValueCount > 0 -> { put("kind", "string_list"); put("value", buildJsonArray { argument.stringListValueList.forEach { add(JsonPrimitive(it)) } }) }
                                            else -> when (argument.valueCase) {
                                            Worker.ApplicationArgument.ValueCase.STRING_VALUE -> { put("kind", "string"); put("value", argument.stringValue) }
                                            Worker.ApplicationArgument.ValueCase.BOOLEAN_VALUE -> { put("kind", "boolean"); put("value", argument.booleanValue) }
                                            Worker.ApplicationArgument.ValueCase.INTEGER_VALUE -> { put("kind", "integer"); put("value", argument.integerValue) }
                                            else -> error("application argument value is required")
                                        }
                                        }
                                    })
                                })
                            }
                        })
                        put("precision", application.precision); put("freshness", application.freshness); put("completeness", application.completeness); put("provenance", json(provenance))
                    })
                }
            })
            put("occurrences", occurrences)
            put("references", buildJsonArray {
                value.referencesList.forEach { edge ->
                    require(edge.sourceOccurrenceIndex in 0 until occurrences.size) {
                        "reference occurrence index out of range"
                    }
                    add(buildJsonObject {
                        put("source", occurrences[edge.sourceOccurrenceIndex])
                        put("target", edge.targetSymbolId)
                        put("precision", edge.precision)
                    })
                }
            })
            put("calls", buildJsonArray {
                value.callsList.forEach { edge ->
                    require(edge.sourceOccurrenceIndex in 0 until occurrences.size) {
                        "call occurrence index out of range"
                    }
                    add(buildJsonObject {
                        put("source", occurrences[edge.sourceOccurrenceIndex])
                        put("target", edge.targetSymbolId)
                        put("caller", nullable(edge.callerSymbolId, edge.hasCallerSymbolId()))
                        put("precision", edge.precision)
                    })
                }
            })
            put("hierarchy", buildJsonArray {
                value.hierarchyList.forEach { edge ->
                    add(buildJsonObject {
                        put("subtype", edge.subtypeSymbolId)
                        put("supertype", edge.supertypeSymbolId)
                        put("precision", edge.precision)
                        put("provenance", json(factProvenance(edge.provenanceIndex)))
                    })
                }
            })
            put("types", buildJsonArray {
                value.typesList.forEach { type ->
                    val factProvenance = factProvenance(type.provenanceIndex)
                    add(buildJsonObject {
                        put("id", type.id)
                        put("language", type.language)
                        put("display", type.display)
                        put("backend_key", if (type.hasBackendKey()) buildJsonObject {
                            put("backend", factProvenance.backend)
                            put("schema_version", type.backendSchemaVersion)
                            put("value", type.backendKey)
                        } else JsonNull)
                        put("freshness", type.freshness)
                        put("completeness", type.completeness)
                        put("provenance", json(factProvenance))
                    })
                }
            })
            put("diagnostics", buildJsonArray {
                value.diagnosticsList.forEach { diagnostic ->
                    require(diagnostic.sourceUnitIndex == 0) { "diagnostic source index out of range" }
                    add(buildJsonObject {
                        put("source_unit", value.sourceUnit.id)
                        put("range", if (diagnostic.hasRange()) json(diagnostic.range) else JsonNull)
                        put("severity", diagnostic.severity)
                        put("code", nullable(diagnostic.code, diagnostic.hasCode()))
                        put("message", diagnostic.message)
                        put("freshness", diagnostic.freshness)
                        put("completeness", diagnostic.completeness)
                        put("provenance", json(factProvenance(diagnostic.provenanceIndex)))
                    })
                }
            })
            put("completeness", value.completeness)
            put("provenance", json(factProvenance(value.provenanceIndex)))
        }
    }
}
