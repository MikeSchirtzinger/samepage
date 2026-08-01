//! Binary frame codec for the canvas WebSocket.
//!
//! One WS connection multiplexes yrs sync messages and bulk blobs; the frame
//! type byte in a fixed 4-byte preamble tells them apart. WS messages are
//! already length-delimited, so frames carry no length prefix.
//!
//! ## The alignment invariant
//!
//! A blob's payload starts at byte offset [`BLOB_HEADER_LEN`] (40, a multiple
//! of 8) within the message. If the receiver lands the message in an 8-byte
//! aligned buffer ([`crate::blob::AlignedBytes`] — NOT a plain `Vec<u8>`),
//! the f32/u32 payload can be reinterpreted in place with zero parse and zero
//! repack, then handed straight to a GPU `write_buffer`. [`decode_frame`]
//! checks the actual pointer alignment and fails loudly on misaligned input
//! rather than letting a `bytemuck` cast panic later.
//!
//! ```text
//! Blob frame layout (offsets in bytes):
//!   0  magic 0xA6      1  version 0x01    2  frame_type 0x02   3  flags
//!   4  dtype tag       5  ndim            6..8   reserved (0)
//!   8..16  blob_id u64 LE
//!  16..20  generation u32 LE              20..24 element_count u32 LE
//!  24..28  dim0 u32 LE                    28..32 dim1 u32 LE
//!  32..40  reserved (0)
//!  40..    payload  (element_count * dtype.size() bytes, 8-aligned start)
//! ```

use crate::blob::{AlignedBytes, DType};
use std::mem::size_of;

pub const FRAME_MAGIC: u8 = 0xA6;
pub const FRAME_VERSION: u8 = 0x01;
/// Preamble shared by every frame: magic, version, frame type, flags.
pub const PREAMBLE_LEN: usize = 4;
/// Full blob header; the payload starts here. Must stay a multiple of 8.
pub const BLOB_HEADER_LEN: usize = 40;
/// BlobRequest frame: preamble + 4 reserved + blob_id u64 + generation u32.
pub const BLOB_REQUEST_LEN: usize = 20;
/// BlobAck frame: BlobRequest + 1 status byte.
pub const BLOB_ACK_LEN: usize = 21;

const _: () = assert!(BLOB_HEADER_LEN.is_multiple_of(8));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    /// Payload is one or more yrs sync messages (`Message::encode_v1` bytes).
    Sync = 0x01,
    /// Bulk geometry payload, header above.
    Blob = 0x02,
    /// "Send me blob_id (I have `generation`, or nothing if 0)."
    BlobRequest = 0x03,
    /// Writer's answer to a stale/unknown request.
    BlobAck = 0x04,
}

impl FrameType {
    fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0x01 => Some(FrameType::Sync),
            0x02 => Some(FrameType::Blob),
            0x03 => Some(FrameType::BlobRequest),
            0x04 => Some(FrameType::BlobAck),
            _ => None,
        }
    }
}

/// Decoded metadata of a blob frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobHeader {
    pub dtype: DType,
    pub ndim: u8,
    pub blob_id: u64,
    pub generation: u32,
    /// Total scalar count (`dim0 * dim1` for 2-D shapes).
    pub element_count: u32,
    pub shape: [u32; 2],
}

impl BlobHeader {
    pub fn payload_len(&self) -> Result<usize, CodecError> {
        (self.element_count as usize)
            .checked_mul(self.dtype.size())
            .ok_or(CodecError::PayloadLengthOverflow)
    }
}

/// A decoded frame borrowing from the receive buffer — no payload copies.
#[derive(Debug, PartialEq)]
pub enum Frame<'a> {
    Sync(&'a [u8]),
    Blob {
        header: BlobHeader,
        payload: &'a [u8],
    },
    BlobRequest {
        blob_id: u64,
        generation: u32,
    },
    BlobAck {
        blob_id: u64,
        generation: u32,
        status: u8,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("frame too short ({0} bytes)")]
    Truncated(usize),
    #[error("bad magic 0x{0:02x}")]
    BadMagic(u8),
    #[error("unsupported frame version {0}")]
    BadVersion(u8),
    #[error("unsupported frame flags 0x{0:02x}")]
    UnsupportedFlags(u8),
    #[error("unknown frame type 0x{0:02x}")]
    BadFrameType(u8),
    #[error("reserved byte at offset {offset} must be zero, got 0x{value:02x}")]
    NonZeroReserved { offset: usize, value: u8 },
    #[error("unknown dtype tag {0}")]
    BadDType(u8),
    #[error("fixed-size frame length {got} != expected {want}")]
    FrameLen { got: usize, want: usize },
    #[error("blob payload length {got} != element_count * dtype size ({want})")]
    PayloadLen { got: usize, want: usize },
    #[error("blob element count times dtype size overflows usize")]
    PayloadLengthOverflow,
    #[error(
        "blob payload misaligned for dtype (needs {needs}-byte alignment); \
         receive into AlignedBytes, not Vec<u8>"
    )]
    Misaligned { needs: usize },
}

fn preamble(frame_type: FrameType) -> [u8; PREAMBLE_LEN] {
    [
        FRAME_MAGIC,
        FRAME_VERSION,
        frame_type as u8,
        0, // flags: bit0 reserved for a future v2 update encoding
    ]
}

fn write_bytes(buf: &mut [u8], at: usize, bytes: &[u8]) -> Result<(), CodecError> {
    let buffer_len = buf.len();
    let end = at
        .checked_add(bytes.len())
        .ok_or(CodecError::Truncated(buffer_len))?;
    let destination = buf
        .get_mut(at..end)
        .ok_or(CodecError::Truncated(buffer_len))?;
    destination.copy_from_slice(bytes);
    Ok(())
}

fn write_byte(buf: &mut [u8], at: usize, byte: u8) -> Result<(), CodecError> {
    let buffer_len = buf.len();
    let destination = buf.get_mut(at).ok_or(CodecError::Truncated(buffer_len))?;
    *destination = byte;
    Ok(())
}

/// Wrap yrs sync-message bytes in a Sync frame.
pub fn encode_sync(sync_payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(PREAMBLE_LEN + sync_payload.len());
    out.extend_from_slice(&preamble(FrameType::Sync));
    out.extend_from_slice(sync_payload);
    out
}

/// Encode a blob frame. The returned buffer is 8-byte aligned, so the payload
/// (at offset 40) is correctly aligned for in-place reinterpretation on the
/// way out as well as on the way in.
///
pub fn encode_blob(header: &BlobHeader, payload: &[u8]) -> Result<AlignedBytes, CodecError> {
    let expected = header.payload_len()?;
    if payload.len() != expected {
        return Err(CodecError::PayloadLen {
            got: payload.len(),
            want: expected,
        });
    }
    let frame_len = BLOB_HEADER_LEN
        .checked_add(payload.len())
        .ok_or(CodecError::PayloadLengthOverflow)?;
    let mut out = AlignedBytes::zeroed(frame_len);
    let buf = out.as_bytes_mut();
    write_bytes(buf, 0, &preamble(FrameType::Blob))?;
    write_byte(buf, 4, header.dtype.tag())?;
    write_byte(buf, 5, header.ndim)?;
    // 6..8 reserved
    write_bytes(buf, 8, &header.blob_id.to_le_bytes())?;
    write_bytes(buf, 16, &header.generation.to_le_bytes())?;
    write_bytes(buf, 20, &header.element_count.to_le_bytes())?;
    let [dim0, dim1] = header.shape;
    write_bytes(buf, 24, &dim0.to_le_bytes())?;
    write_bytes(buf, 28, &dim1.to_le_bytes())?;
    // 32..40 reserved
    write_bytes(buf, BLOB_HEADER_LEN, payload)?;
    Ok(out)
}

pub fn encode_blob_request(blob_id: u64, generation: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(BLOB_REQUEST_LEN);
    out.extend_from_slice(&preamble(FrameType::BlobRequest));
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&blob_id.to_le_bytes());
    out.extend_from_slice(&generation.to_le_bytes());
    out
}

pub fn encode_blob_ack(blob_id: u64, generation: u32, status: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(BLOB_ACK_LEN);
    out.extend_from_slice(&preamble(FrameType::BlobAck));
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&blob_id.to_le_bytes());
    out.extend_from_slice(&generation.to_le_bytes());
    out.push(status);
    out
}

fn read_u32(buf: &[u8], at: usize) -> Result<u32, CodecError> {
    let end = at
        .checked_add(size_of::<u32>())
        .ok_or(CodecError::Truncated(buf.len()))?;
    let bytes = buf.get(at..end).ok_or(CodecError::Truncated(buf.len()))?;
    let mut value = [0; size_of::<u32>()];
    value.copy_from_slice(bytes);
    Ok(u32::from_le_bytes(value))
}

fn read_u64(buf: &[u8], at: usize) -> Result<u64, CodecError> {
    let end = at
        .checked_add(size_of::<u64>())
        .ok_or(CodecError::Truncated(buf.len()))?;
    let bytes = buf.get(at..end).ok_or(CodecError::Truncated(buf.len()))?;
    let mut value = [0; size_of::<u64>()];
    value.copy_from_slice(bytes);
    Ok(u64::from_le_bytes(value))
}

fn read_u8(buf: &[u8], at: usize) -> Result<u8, CodecError> {
    buf.get(at).copied().ok_or(CodecError::Truncated(buf.len()))
}

fn require_zero_range(buf: &[u8], range: std::ops::Range<usize>) -> Result<(), CodecError> {
    let bytes = buf
        .get(range.clone())
        .ok_or(CodecError::Truncated(buf.len()))?;
    for (relative, value) in bytes.iter().copied().enumerate() {
        if value != 0 {
            let offset = range
                .start
                .checked_add(relative)
                .ok_or(CodecError::Truncated(buf.len()))?;
            return Err(CodecError::NonZeroReserved { offset, value });
        }
    }
    Ok(())
}

/// Decode one frame from a receive buffer. Borrows the payload — the caller
/// keeps the buffer alive (and aligned: see module docs) for as long as the
/// payload slice is used.
pub fn decode_frame(buf: &[u8]) -> Result<Frame<'_>, CodecError> {
    if buf.len() < PREAMBLE_LEN {
        return Err(CodecError::Truncated(buf.len()));
    }
    let magic = read_u8(buf, 0)?;
    if magic != FRAME_MAGIC {
        return Err(CodecError::BadMagic(magic));
    }
    let version = read_u8(buf, 1)?;
    if version != FRAME_VERSION {
        return Err(CodecError::BadVersion(version));
    }
    let flags = read_u8(buf, 3)?;
    if flags != 0 {
        return Err(CodecError::UnsupportedFlags(flags));
    }
    let frame_type_tag = read_u8(buf, 2)?;
    let frame_type =
        FrameType::from_tag(frame_type_tag).ok_or(CodecError::BadFrameType(frame_type_tag))?;

    match frame_type {
        FrameType::Sync => Ok(Frame::Sync(
            buf.get(PREAMBLE_LEN..)
                .ok_or(CodecError::Truncated(buf.len()))?,
        )),
        FrameType::Blob => {
            if buf.len() < BLOB_HEADER_LEN {
                return Err(CodecError::Truncated(buf.len()));
            }
            require_zero_range(buf, 6..8)?;
            require_zero_range(buf, 32..40)?;
            let dtype_tag = read_u8(buf, 4)?;
            let dtype = DType::from_tag(dtype_tag).ok_or(CodecError::BadDType(dtype_tag))?;
            let header = BlobHeader {
                dtype,
                ndim: read_u8(buf, 5)?,
                blob_id: read_u64(buf, 8)?,
                generation: read_u32(buf, 16)?,
                element_count: read_u32(buf, 20)?,
                shape: [read_u32(buf, 24)?, read_u32(buf, 28)?],
            };
            let payload = buf
                .get(BLOB_HEADER_LEN..)
                .ok_or(CodecError::Truncated(buf.len()))?;
            let expected = header.payload_len()?;
            if payload.len() != expected {
                return Err(CodecError::PayloadLen {
                    got: payload.len(),
                    want: expected,
                });
            }
            let needs = dtype.align();
            if !(payload.as_ptr() as usize).is_multiple_of(needs) {
                return Err(CodecError::Misaligned { needs });
            }
            Ok(Frame::Blob { header, payload })
        }
        FrameType::BlobRequest => {
            if buf.len() != BLOB_REQUEST_LEN {
                return Err(CodecError::FrameLen {
                    got: buf.len(),
                    want: BLOB_REQUEST_LEN,
                });
            }
            require_zero_range(buf, 4..8)?;
            Ok(Frame::BlobRequest {
                blob_id: read_u64(buf, 8)?,
                generation: read_u32(buf, 16)?,
            })
        }
        FrameType::BlobAck => {
            if buf.len() != BLOB_ACK_LEN {
                return Err(CodecError::FrameLen {
                    got: buf.len(),
                    want: BLOB_ACK_LEN,
                });
            }
            require_zero_range(buf, 4..8)?;
            Ok(Frame::BlobAck {
                blob_id: read_u64(buf, 8)?,
                generation: read_u32(buf, 16)?,
                status: read_u8(buf, 20)?,
            })
        }
    }
}
