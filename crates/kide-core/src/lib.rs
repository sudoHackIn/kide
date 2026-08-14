//! Persistent, frontend-independent KIDE primitives.
//!
//! The canonical model intentionally persists graph facts, not a universal AST
//! or compiler object graph. Language and build-system workers remain
//! disposable compute processes as defined by ADR 0001.

mod artifact_cache;
pub mod worker_proto {
    include!(concat!(env!("OUT_DIR"), "/kide.worker.v1.rs"));
}

/// Length-delimited protobuf frames for future cold-worker transport.
pub mod worker_framing {
    use prost::Message;
    use thiserror::Error;

    use crate::worker_proto::Envelope;

    #[derive(Debug, Error)]
    pub enum FrameError {
        #[error("invalid protobuf frame length")]
        InvalidLength,
        #[error("protobuf frame length does not match its payload")]
        LengthMismatch,
        #[error("protobuf payload failed to decode: {0}")]
        Decode(#[from] prost::DecodeError),
    }

    pub fn encode(envelope: &Envelope) -> Vec<u8> {
        let mut frame = Vec::with_capacity(envelope.encoded_len() + 10);
        let mut length = envelope.encoded_len() as u64;
        while length >= 0x80 {
            frame.push((length as u8) | 0x80);
            length >>= 7;
        }
        frame.push(length as u8);
        envelope
            .encode(&mut frame)
            .expect("Vec reserves enough space");
        frame
    }

    pub fn decode(frame: &[u8]) -> Result<Envelope, FrameError> {
        let mut length = 0_u64;
        let mut shift = 0;
        let mut offset = 0;
        for byte in frame {
            length |= u64::from(byte & 0x7f) << shift;
            offset += 1;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift >= 64 {
                return Err(FrameError::InvalidLength);
            }
        }
        let payload = frame.get(offset..).ok_or(FrameError::InvalidLength)?;
        if usize::try_from(length).ok() != Some(payload.len()) {
            return Err(FrameError::LengthMismatch);
        }
        Ok(Envelope::decode(payload)?)
    }
}

pub mod worker_proto_adapter {
    use thiserror::Error;

    use crate::{
        AnalyzeBatchRequest, ArtifactDescriptor, ArtifactDiscoveryRequest,
        ArtifactDiscoveryResponse, BuildSystem, Component, ComponentId, DependencyEdge,
        DependencyTarget, Fingerprint, Language, ProjectManifest, Provenance, SourceOrigin,
        SourceSet, SourceUnit, SourceUnitId, Toolchain, WorkspaceId, WorkspacePath, worker_proto,
    };

    #[derive(Debug, Error)]
    pub enum AdapterError {
        #[error("missing required protobuf field {0}")]
        Missing(&'static str),
        #[error("unsupported protobuf enum value {0}")]
        Unsupported(String),
    }

    fn language(value: String) -> Language {
        match value.as_str() {
            "kotlin" => Language::Kotlin,
            "java" => Language::Java,
            "rust" => Language::Rust,
            "typescript" => Language::TypeScript,
            other => Language::Other(other.to_owned()),
        }
    }

    fn build_system(value: String) -> BuildSystem {
        match value.as_str() {
            "gradle" => BuildSystem::Gradle,
            "maven" => BuildSystem::Maven,
            "cargo" => BuildSystem::Cargo,
            "npm" => BuildSystem::Npm,
            "bazel" => BuildSystem::Bazel,
            "filesystem" => BuildSystem::Filesystem,
            other => BuildSystem::Other(other.to_owned()),
        }
    }

    fn provenance(value: worker_proto::Provenance) -> Provenance {
        Provenance {
            backend: value.backend,
            backend_version: value.backend_version,
            protocol_version: value.protocol_version,
            analysis_options: Fingerprint::new(value.analysis_options_fingerprint),
        }
    }

    fn proto_provenance(value: &Provenance) -> worker_proto::Provenance {
        worker_proto::Provenance {
            backend: value.backend.clone(),
            backend_version: value.backend_version.clone(),
            protocol_version: value.protocol_version,
            analysis_options_fingerprint: value.analysis_options.as_str().to_owned(),
        }
    }

    fn source_origin(value: String) -> Result<SourceOrigin, AdapterError> {
        match value.as_str() {
            "source" => Ok(SourceOrigin::Source),
            "generated" => Ok(SourceOrigin::Generated),
            "dependency" => Ok(SourceOrigin::Dependency),
            other => Err(AdapterError::Unsupported(format!("source origin {other}"))),
        }
    }

    fn proto_language(value: &Language) -> String {
        match value {
            Language::Kotlin => "kotlin".to_owned(),
            Language::Java => "java".to_owned(),
            Language::Rust => "rust".to_owned(),
            Language::TypeScript => "typescript".to_owned(),
            Language::Other(value) => value.clone(),
        }
    }

    fn proto_source_origin(value: &SourceOrigin) -> &'static str {
        match value {
            SourceOrigin::Source => "source",
            SourceOrigin::Generated => "generated",
            SourceOrigin::Dependency => "dependency",
        }
    }

    pub fn source_unit(value: &SourceUnit) -> worker_proto::SourceUnit {
        worker_proto::SourceUnit {
            id: value.id.as_str().to_owned(),
            component: value.component.as_str().to_owned(),
            path: value.path.as_str().to_owned(),
            language: proto_language(&value.language),
            origin: proto_source_origin(&value.origin).to_owned(),
            content: value.content.as_str().to_owned(),
            context: value.context.as_str().to_owned(),
        }
    }

    pub fn decode_source_unit(value: worker_proto::SourceUnit) -> Result<SourceUnit, AdapterError> {
        Ok(SourceUnit {
            id: SourceUnitId::new(value.id),
            component: ComponentId::new(value.component),
            path: WorkspacePath::new(value.path),
            language: language(value.language),
            origin: source_origin(value.origin)?,
            content: Fingerprint::new(value.content),
            context: Fingerprint::new(value.context),
        })
    }

    pub fn analyze_batch_request(value: &AnalyzeBatchRequest) -> worker_proto::AnalyzeBatchRequest {
        worker_proto::AnalyzeBatchRequest {
            workspace: value.workspace.as_str().to_owned(),
            project_fingerprint: value.project_fingerprint.as_str().to_owned(),
            requested_facts: value
                .requested_facts
                .iter()
                .map(|fact| format!("{fact:?}").to_lowercase())
                .collect(),
            source_units: value.source_units.iter().map(source_unit).collect(),
        }
    }

    pub fn decode_manifest(
        value: worker_proto::ProjectManifest,
    ) -> Result<ProjectManifest, AdapterError> {
        let worker_provenance = value
            .provenance
            .ok_or(AdapterError::Missing("manifest.provenance"))?;
        let components = value
            .components
            .into_iter()
            .map(|component| Component {
                id: ComponentId::new(component.id),
                name: component.name,
                build_system: build_system(component.build_system),
                root: WorkspacePath::new(component.root),
                languages: component.languages.into_iter().map(language).collect(),
                configuration: Fingerprint::new(component.configuration_fingerprint),
                source_sets: component
                    .source_sets
                    .into_iter()
                    .map(|set| SourceSet {
                        name: set.name,
                        source_roots: set
                            .source_roots
                            .into_iter()
                            .map(WorkspacePath::new)
                            .collect(),
                        generated_roots: set
                            .generated_roots
                            .into_iter()
                            .map(WorkspacePath::new)
                            .collect(),
                        test: set.test,
                    })
                    .collect(),
                classpath: component
                    .classpath_fingerprints
                    .into_iter()
                    .map(Fingerprint::new)
                    .collect(),
                toolchain: component.toolchain.map(|toolchain| Toolchain {
                    jvm_version: toolchain.jvm_version,
                    gradle_version: toolchain.gradle_version,
                    kotlin_version: toolchain.kotlin_version,
                }),
                compiler_configuration: component
                    .compiler_configuration_fingerprint
                    .map(Fingerprint::new),
            })
            .collect();
        let dependencies = value
            .dependencies
            .into_iter()
            .map(|edge| {
                let target = edge
                    .target
                    .ok_or(AdapterError::Missing("dependency.target"))?;
                Ok(DependencyEdge {
                    from: ComponentId::new(edge.from_component_id),
                    target: match target {
                        worker_proto::dependency_edge::Target::ComponentId(component) => {
                            DependencyTarget::Component {
                                component: ComponentId::new(component),
                            }
                        }
                        worker_proto::dependency_edge::Target::ArtifactFingerprint(content) => {
                            DependencyTarget::Artifact {
                                content: Fingerprint::new(content),
                            }
                        }
                    },
                    scope: edge.scope,
                })
            })
            .collect::<Result<Vec<_>, AdapterError>>()?;
        Ok(ProjectManifest {
            workspace: WorkspaceId::new(value.workspace),
            root: WorkspacePath::new(value.root),
            components,
            dependencies,
            fingerprint: Fingerprint::new(value.fingerprint),
            provenance: provenance(worker_provenance),
        })
    }

    pub fn discovery_request(
        request: &ArtifactDiscoveryRequest,
    ) -> worker_proto::ArtifactDiscoveryRequest {
        worker_proto::ArtifactDiscoveryRequest {
            workspace_root: request.workspace_root.as_str().to_owned(),
            max_artifacts: request.max_artifacts,
            cursor: request.cursor.clone(),
        }
    }

    pub fn discovery_response(
        response: &ArtifactDiscoveryResponse,
    ) -> worker_proto::ArtifactDiscoveryResponse {
        worker_proto::ArtifactDiscoveryResponse {
            artifacts: response.artifacts.iter().map(descriptor).collect(),
            next_cursor: response.next_cursor.clone(),
        }
    }

    fn descriptor(value: &ArtifactDescriptor) -> worker_proto::ArtifactDescriptor {
        let unit = &value.source_unit;
        worker_proto::ArtifactDescriptor {
            source_unit_id: unit.id.as_str().to_owned(),
            component_id: unit.component.as_str().to_owned(),
            workspace_path: unit.path.as_str().to_owned(),
            content_fingerprint: unit.content.as_str().to_owned(),
            context_fingerprint: unit.context.as_str().to_owned(),
            backend: value.provenance.backend.clone(),
            backend_version: value.provenance.backend_version.clone(),
            worker_protocol_version: value.provenance.protocol_version,
            analysis_options_fingerprint: value.provenance.analysis_options.as_str().to_owned(),
        }
    }

    pub fn decode_descriptor(value: worker_proto::ArtifactDescriptor) -> ArtifactDescriptor {
        ArtifactDescriptor {
            source_unit: SourceUnit {
                id: SourceUnitId::new(value.source_unit_id),
                component: ComponentId::new(value.component_id),
                path: WorkspacePath::new(value.workspace_path),
                language: Language::Java,
                origin: SourceOrigin::Dependency,
                content: Fingerprint::new(value.content_fingerprint),
                context: Fingerprint::new(value.context_fingerprint),
            },
            provenance: Provenance {
                backend: value.backend,
                backend_version: value.backend_version,
                protocol_version: value.worker_protocol_version,
                analysis_options: Fingerprint::new(value.analysis_options_fingerprint),
            },
        }
    }

    pub fn decode_discovery_response(
        value: worker_proto::ArtifactDiscoveryResponse,
    ) -> ArtifactDiscoveryResponse {
        ArtifactDiscoveryResponse {
            artifacts: value.artifacts.into_iter().map(decode_descriptor).collect(),
            next_cursor: value.next_cursor,
        }
    }

    pub fn manifest(value: &ProjectManifest) -> worker_proto::ProjectManifest {
        worker_proto::ProjectManifest {
            workspace: value.workspace.as_str().to_owned(),
            root: value.root.as_str().to_owned(),
            components: value
                .components
                .iter()
                .map(|component| worker_proto::Component {
                    id: component.id.as_str().to_owned(),
                    name: component.name.clone(),
                    build_system: format!("{:?}", component.build_system).to_lowercase(),
                    root: component.root.as_str().to_owned(),
                    languages: component
                        .languages
                        .iter()
                        .map(|language| format!("{:?}", language).to_lowercase())
                        .collect(),
                    configuration_fingerprint: component.configuration.as_str().to_owned(),
                    source_sets: component
                        .source_sets
                        .iter()
                        .map(|set| worker_proto::SourceSet {
                            name: set.name.clone(),
                            source_roots: set
                                .source_roots
                                .iter()
                                .map(|path| path.as_str().to_owned())
                                .collect(),
                            generated_roots: set
                                .generated_roots
                                .iter()
                                .map(|path| path.as_str().to_owned())
                                .collect(),
                            test: set.test,
                        })
                        .collect(),
                    classpath_fingerprints: component
                        .classpath
                        .iter()
                        .map(|fingerprint| fingerprint.as_str().to_owned())
                        .collect(),
                    toolchain: component.toolchain.as_ref().map(|toolchain| {
                        worker_proto::Toolchain {
                            jvm_version: toolchain.jvm_version.clone(),
                            gradle_version: toolchain.gradle_version.clone(),
                            kotlin_version: toolchain.kotlin_version.clone(),
                        }
                    }),
                    compiler_configuration_fingerprint: component
                        .compiler_configuration
                        .as_ref()
                        .map(|fingerprint| fingerprint.as_str().to_owned()),
                })
                .collect(),
            dependencies: value
                .dependencies
                .iter()
                .map(|edge| worker_proto::DependencyEdge {
                    from_component_id: edge.from.as_str().to_owned(),
                    scope: edge.scope.clone(),
                    target: Some(match &edge.target {
                        DependencyTarget::Component { component } => {
                            worker_proto::dependency_edge::Target::ComponentId(
                                component.as_str().to_owned(),
                            )
                        }
                        DependencyTarget::Artifact { content } => {
                            worker_proto::dependency_edge::Target::ArtifactFingerprint(
                                content.as_str().to_owned(),
                            )
                        }
                    }),
                })
                .collect(),
            fingerprint: value.fingerprint.as_str().to_owned(),
            provenance: Some(proto_provenance(&value.provenance)),
        }
    }
}

#[cfg(test)]
mod worker_framing_tests {
    use crate::{worker_framing, worker_proto};

    #[test]
    fn protobuf_frames_round_trip_a_descriptor_request() {
        let message = worker_proto::Envelope {
            protocol_version: 3,
            request_id: "descriptor-1".to_owned(),
            message: Some(worker_proto::envelope::Message::ArtifactDiscoveryRequest(
                worker_proto::ArtifactDiscoveryRequest {
                    workspace_root: ".".to_owned(),
                    max_artifacts: 8,
                    cursor: Some("cursor-7".to_owned()),
                },
            )),
        };
        assert_eq!(
            worker_framing::decode(&worker_framing::encode(&message)).expect("decodes frame"),
            message
        );
    }
}
mod canonical;
mod discovery;
mod freshness;
mod orchestrator;
mod protocol;
mod query;
mod store;
mod supervisor;

pub use artifact_cache::*;
pub use canonical::*;
pub use discovery::*;
pub use freshness::*;
pub use orchestrator::*;
pub use protocol::*;
pub use query::*;
pub use store::*;
pub use supervisor::*;

/// Version of the normalized records and JSON envelopes owned by KIDE Core.
pub const CANONICAL_SCHEMA_VERSION: u32 = 1;

/// Format of the physical persistent index.
///
/// The storage engine may evolve independently, but a reader must reject a
/// newer incompatible format rather than treating it as fresh data.
pub const INDEX_FORMAT_VERSION: u32 = 3;
