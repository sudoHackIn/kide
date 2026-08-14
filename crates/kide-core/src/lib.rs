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
