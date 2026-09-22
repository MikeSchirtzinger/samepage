//! The `/ws` wire format: y-sync v1 messages, nothing else.
//!
//! A frame is one or more y-protocols sync messages (SyncStep1, SyncStep2,
//! Update). Awareness, auth and custom messages are refused, and so is any
//! frame whose bytes are not exactly a sequence of whole messages: a trailing
//! fragment is an error, never silently dropped. The framing is read directly
//! rather than by comparing a re-encoding, because a state vector re-encodes
//! in hash-map order and valid peers would be refused.

use thiserror::Error;
use yrs::encoding::read::{Cursor, Read};
use yrs::sync::protocol::{
    MSG_AUTH, MSG_AWARENESS, MSG_QUERY_AWARENESS, MSG_SYNC, MSG_SYNC_STEP_1, MSG_SYNC_STEP_2,
    MSG_SYNC_UPDATE,
};
use yrs::sync::{Message, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::StateVector;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SyncError {
    #[error("frame is not a sequence of y-sync messages: {0}")]
    Malformed(String),
    #[error("{0} messages are not part of the document protocol")]
    Unsupported(&'static str),
    #[error("frame is empty")]
    Empty,
}

/// One message the document acts on.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// The peer's state vector: answer with what it is missing.
    Step1(StateVector),
    /// Document content (a SyncStep2 or an Update; they are handled alike).
    Update(Vec<u8>),
}

pub fn parse(frame: &[u8]) -> Result<Vec<Incoming>, SyncError> {
    if frame.is_empty() {
        return Err(SyncError::Empty);
    }
    // The y-sync framing read directly: message tag, sync subtype, one
    // length-prefixed payload. Every byte of the frame must be consumed by
    // whole messages; a truncated tail is an error, never dropped.
    let malformed = |error: yrs::encoding::read::Error| SyncError::Malformed(error.to_string());
    let mut cursor = Cursor::new(frame);
    let mut incoming = Vec::new();
    while cursor.next < frame.len() {
        let tag: u8 = cursor.read_var().map_err(malformed)?;
        match tag {
            MSG_SYNC => {
                let subtype: u8 = cursor.read_var().map_err(malformed)?;
                let payload = cursor.read_buf().map_err(malformed)?.to_vec();
                match subtype {
                    MSG_SYNC_STEP_1 => incoming.push(Incoming::Step1(
                        StateVector::decode_v1(&payload).map_err(malformed)?,
                    )),
                    MSG_SYNC_STEP_2 | MSG_SYNC_UPDATE => incoming.push(Incoming::Update(payload)),
                    other => {
                        return Err(SyncError::Malformed(format!(
                            "unknown sync message {other}"
                        )))
                    }
                }
            }
            MSG_AWARENESS | MSG_QUERY_AWARENESS => return Err(SyncError::Unsupported("awareness")),
            MSG_AUTH => return Err(SyncError::Unsupported("auth")),
            _ => return Err(SyncError::Unsupported("custom")),
        }
    }
    Ok(incoming)
}

pub fn step1(sv: &StateVector) -> Vec<u8> {
    Message::Sync(SyncMessage::SyncStep1(sv.clone())).encode_v1()
}

pub fn step2(update: Vec<u8>) -> Vec<u8> {
    Message::Sync(SyncMessage::SyncStep2(update)).encode_v1()
}

pub fn update(update: Vec<u8>) -> Vec<u8> {
    Message::Sync(SyncMessage::Update(update)).encode_v1()
}
