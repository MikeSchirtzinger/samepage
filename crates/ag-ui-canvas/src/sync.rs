//! y-sync protocol driver over a [`Scene`].
//!
//! The Sync frame payload is standard y-sync bytes (`yrs::sync::Message`
//! v1 encoding), so a future bridge to y-websocket peers only needs to strip
//! the 4-byte frame preamble. Handshake:
//!
//! ```text
//! A connects:  A → B  SyncStep1(A's state vector)
//!              B → A  SyncStep2(everything A is missing)   ← differential
//!              B → A  SyncStep1(B's state vector)           (symmetric)
//!              A → B  SyncStep2(everything B is missing)
//! steady state: every committed transaction → Update(bytes) both ways
//! ```
//!
//! Awareness/Auth messages are not implemented in v1 and are rejected. A peer
//! must not mistake an accepted frame for working presence or authentication.

use yrs::sync::{Error as YSyncError, Message, MessageReader, SyncMessage};
use yrs::updates::decoder::DecoderV1;
use yrs::updates::encoder::Encode;

use crate::blob::BlobStore;
use crate::codec::{encode_blob, encode_sync, BlobHeader, CodecError};
use crate::scene::{Scene, SceneError};

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("sync message decode failed: {0}")]
    Protocol(String),
    #[error(transparent)]
    Scene(#[from] SceneError),
    #[error("candidate scene validation failed: {0}")]
    Validation(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error(transparent)]
    Sync(#[from] SyncError),
    #[error(transparent)]
    Scene(#[from] SceneError),
    #[error("scene references missing blob {0}")]
    MissingBlob(u64),
    #[error("blob {blob_id} byte length {bytes} is not aligned to element size {element_size}")]
    MisalignedBlob {
        blob_id: u64,
        bytes: usize,
        element_size: usize,
    },
    #[error("blob {0} element count exceeds u32")]
    ElementCountOverflow(u64),
    #[error("blob {0} does not satisfy its authoritative scene reference")]
    ReferenceMismatch(u64),
    #[error("scene has conflicting references for blob {0}")]
    ConflictingReferences(u64),
    #[error("blob {blob_id} replay encoding failed: {source}")]
    Encoding {
        blob_id: u64,
        #[source]
        source: CodecError,
    },
}

impl From<YSyncError> for SyncError {
    fn from(e: YSyncError) -> Self {
        SyncError::Protocol(e.to_string())
    }
}

impl From<yrs::encoding::read::Error> for SyncError {
    fn from(e: yrs::encoding::read::Error) -> Self {
        SyncError::Protocol(e.to_string())
    }
}

/// First message a peer sends after connecting: SyncStep1 with our state
/// vector. The reply (SyncStep2) carries exactly what we're missing.
pub fn greeting(scene: &Scene) -> Result<Vec<u8>, SyncError> {
    Ok(Message::Sync(SyncMessage::SyncStep1(scene.state_vector()?)).encode_v1())
}

/// Wrap freshly committed local update bytes (from [`Scene::on_update`]) as a
/// y-sync Update message.
pub fn update_message(update_v1: &[u8]) -> Vec<u8> {
    Message::Sync(SyncMessage::Update(update_v1.to_vec())).encode_v1()
}

/// Build an atomic reconnect replay: sync greeting, full scene, then every
/// blob referenced by that exact scene snapshot. Any missing, malformed, or
/// conflicting geometry fails the whole replay so callers never send a
/// visually incomplete authoritative state.
pub fn authoritative_replay(scene: &Scene, blobs: &BlobStore) -> Result<Vec<Vec<u8>>, ReplayError> {
    let mut frames = vec![
        encode_sync(&greeting(scene)?),
        encode_sync(&update_message(&scene.encode_full()?)),
    ];
    let objects = scene.snapshot()?;
    let mut references = std::collections::BTreeMap::new();
    for blob_ref in objects.iter().filter_map(|object| object.blob_ref) {
        match references.get(&blob_ref.blob_id) {
            Some(existing) if existing != &blob_ref => {
                return Err(ReplayError::ConflictingReferences(blob_ref.blob_id));
            }
            Some(_) => continue,
            None => {
                references.insert(blob_ref.blob_id, blob_ref);
            }
        }
    }
    for (blob_id, blob_ref) in references {
        let entry = blobs
            .get(blob_id)
            .ok_or(ReplayError::MissingBlob(blob_id))?;
        let element_size = entry.dtype.size();
        if !entry.bytes.len().is_multiple_of(element_size) {
            return Err(ReplayError::MisalignedBlob {
                blob_id,
                bytes: entry.bytes.len(),
                element_size,
            });
        }
        let element_count = u32::try_from(entry.bytes.len() / element_size)
            .map_err(|_| ReplayError::ElementCountOverflow(blob_id))?;
        let header = BlobHeader {
            dtype: entry.dtype,
            ndim: 2,
            blob_id,
            generation: entry.generation,
            element_count,
            shape: entry.shape,
        };
        if !blob_ref.accepts_header(&header) {
            return Err(ReplayError::ReferenceMismatch(blob_id));
        }
        frames.push(
            encode_blob(&header, entry.bytes.as_bytes())
                .map_err(|source| ReplayError::Encoding { blob_id, source })?
                .as_bytes()
                .to_vec(),
        );
    }
    Ok(frames)
}

/// Handle one inbound Sync frame payload (one or more packed y-sync
/// messages). Applies updates to `scene`; returns reply payloads to send
/// back (e.g. SyncStep2 answering a SyncStep1).
///
/// Call this from the network task, never from inside a Doc observer —
/// applying updates opens a write transaction.
pub fn handle_payload(scene: &Scene, payload: &[u8]) -> Result<Vec<Vec<u8>>, SyncError> {
    handle_payload_validated(scene, payload, |_| Ok(()))
}

/// Apply a sync payload only after both the core scene schema and a
/// caller-supplied authority check accept a disposable candidate replica.
/// Servers use the extra check to ensure client updates cannot introduce blob
/// references absent from the server-owned [`BlobStore`].
pub fn handle_payload_validated(
    scene: &Scene,
    payload: &[u8],
    validate: impl Fn(&Scene) -> Result<(), String>,
) -> Result<Vec<Vec<u8>>, SyncError> {
    let mut decoder = DecoderV1::from(payload);
    let candidate = Scene::from_state(&scene.encode_full()?)?;
    let mut replies = Vec::new();
    let mut has_update = false;
    for message in MessageReader::new(&mut decoder) {
        match message? {
            Message::Sync(SyncMessage::SyncStep1(remote_sv)) => {
                let diff = candidate.encode_diff(&remote_sv)?;
                replies.push(Message::Sync(SyncMessage::SyncStep2(diff)).encode_v1());
            }
            Message::Sync(SyncMessage::SyncStep2(update))
            | Message::Sync(SyncMessage::Update(update)) => {
                candidate.apply_update(&update)?;
                has_update = true;
            }
            Message::AwarenessQuery | Message::Awareness(_) => {
                return Err(SyncError::Protocol(
                    "awareness/presence messages are not implemented".to_string(),
                ));
            }
            Message::Auth(_) => {
                return Err(SyncError::Protocol(
                    "authentication messages are not implemented".to_string(),
                ));
            }
            Message::Custom(tag, _) => {
                return Err(SyncError::Protocol(format!(
                    "custom y-sync message tag {tag} is not implemented"
                )));
            }
        }
    }
    // Validate the final state of the entire packed payload before applying a
    // single byte to the live Doc. This prevents a valid update followed by an
    // invalid update from committing a partial prefix or firing observers.
    candidate.snapshot()?;
    validate(&candidate).map_err(SyncError::Validation)?;
    if has_update {
        let live_state = scene.state_vector()?;
        let accepted_update = candidate.encode_diff(&live_state)?;
        scene.apply_update(&accepted_update)?;
    }
    Ok(replies)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob::{AlignedBytes, BlobEntry, DType};
    use crate::scene::{Author, BlobRef};
    use std::sync::{Arc, Mutex};
    use yrs::{Map, Out, ReadTxn, Transact};

    fn particle_scene() -> Scene {
        let mut scene = Scene::new();
        let id = scene
            .create_object("particles", Author::Agent)
            .expect("create particle");
        scene
            .set_blob_ref(
                &id,
                BlobRef {
                    blob_id: 7,
                    generation: 1,
                    count: 4,
                    dtype: DType::F32,
                },
            )
            .expect("set blob reference");
        scene
    }

    #[test]
    fn authoritative_replay_is_atomic_and_validates_geometry() {
        let scene = particle_scene();
        assert!(matches!(
            authoritative_replay(&scene, &BlobStore::new()),
            Err(ReplayError::MissingBlob(7))
        ));

        let mut mismatched = BlobStore::new();
        mismatched.insert(
            7,
            BlobEntry {
                dtype: DType::F32,
                shape: [1, 4],
                generation: 1,
                bytes: AlignedBytes::from_bytes(&[0; 16]),
            },
        );
        assert!(matches!(
            authoritative_replay(&scene, &mismatched),
            Err(ReplayError::ReferenceMismatch(7))
        ));

        let mut valid = BlobStore::new();
        valid.insert(
            7,
            BlobEntry {
                dtype: DType::F32,
                shape: [2, 2],
                generation: 1,
                bytes: AlignedBytes::from_bytes(&[0; 16]),
            },
        );
        assert_eq!(
            authoritative_replay(&scene, &valid)
                .expect("valid replay")
                .len(),
            3
        );
    }

    #[test]
    fn unsupported_presence_auth_and_custom_messages_fail_loudly() {
        let scene = Scene::new();
        let cases = [
            (
                Message::AwarenessQuery.encode_v1(),
                "awareness/presence messages are not implemented",
            ),
            (
                Message::Awareness(yrs::sync::AwarenessUpdate {
                    clients: std::collections::HashMap::new(),
                })
                .encode_v1(),
                "awareness/presence messages are not implemented",
            ),
            (
                Message::Auth(None).encode_v1(),
                "authentication messages are not implemented",
            ),
            (
                Message::Custom(9, vec![1, 2, 3]).encode_v1(),
                "custom y-sync message tag 9 is not implemented",
            ),
        ];

        for (payload, expected) in cases {
            let error =
                handle_payload(&scene, &payload).expect_err("unsupported message must be rejected");
            assert!(
                error.to_string().contains(expected),
                "unexpected rejection: {error}"
            );
        }
    }

    #[test]
    fn unsupported_message_rejects_the_entire_packed_payload() {
        let target = Scene::new();
        let mut remote = Scene::new();
        remote
            .create_object("disc", Author::Agent)
            .expect("create remote object");

        let mut payload = update_message(&remote.encode_full().expect("encode remote update"));
        payload.extend_from_slice(&Message::Auth(None).encode_v1());
        let error = handle_payload(&target, &payload)
            .expect_err("trailing unsupported auth must reject the packed payload");
        assert!(error.to_string().contains("authentication"));
        assert!(
            target
                .snapshot()
                .expect("read target after rejection")
                .is_empty(),
            "a rejected packed payload must not commit its valid prefix"
        );
    }

    #[test]
    fn packed_payload_rejection_has_no_partial_mutation_or_observer_event() {
        let mut target = Scene::new();
        let id = target
            .create_object("disc", Author::Human)
            .expect("create target object");
        target
            .set_prop(&id, "x", crate::scene::PropValue::Num(0.0))
            .expect("set baseline x");
        let remote =
            Scene::from_state(&target.encode_full().expect("encode target")).expect("clone target");

        let remote_updates = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let captured = remote_updates.clone();
        let _remote_subscription = remote
            .on_update(move |update| {
                captured
                    .lock()
                    .expect("remote update lock")
                    .push(update.to_vec());
            })
            .expect("observe remote");
        remote
            .set_prop(&id, "x", crate::scene::PropValue::Num(1.0))
            .expect("valid prefix update");
        {
            let mut txn = remote.doc().transact_mut();
            let objects = txn.get_map("objects").expect("objects root");
            let Out::YMap(object) = objects.get(&txn, id.as_str()).expect("remote object") else {
                panic!("remote object must be a map");
            };
            object.insert(&mut txn, "z_index", f64::NAN);
        }
        let updates = remote_updates.lock().expect("captured updates");
        assert_eq!(updates.len(), 2);
        let mut payload = update_message(&updates[0]);
        payload.extend_from_slice(&update_message(&updates[1]));

        let target_updates = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let observed = target_updates.clone();
        let _target_subscription = target
            .on_update(move |update| {
                observed
                    .lock()
                    .expect("target update lock")
                    .push(update.to_vec());
            })
            .expect("observe target");

        assert!(handle_payload(&target, &payload).is_err());
        assert_eq!(
            target
                .get_prop(&id, "x")
                .expect("read target x")
                .and_then(|value| value.as_f64()),
            Some(0.0)
        );
        assert!(target.snapshot().is_ok());
        assert!(target_updates.lock().expect("target updates").is_empty());
    }
}
