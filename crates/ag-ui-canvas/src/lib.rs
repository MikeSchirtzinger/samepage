//! Shared-canvas state layer for AG-UI: a yrs CRDT scene graph that humans
//! and agents co-edit, plus an aligned binary frame codec for the WebSocket
//! fast path.
//!
//! Three channels:
//! 1. **Semantic events** — JSON AG-UI over SSE (`ag-ui-core`, unchanged);
//!    builders in [`events`].
//! 2. **Scene mutations** — yrs binary updates ([`scene`], [`sync`]).
//! 3. **Bulk geometry** — generation-versioned blobs outside the CRDT
//!    ([`blob`], [`codec`]), 8-byte aligned for zero-parse GPU upload.

pub mod blob;
pub mod codec;
pub mod events;
pub mod ids;
pub mod scene;
pub mod sync;

pub use blob::{AlignedBytes, BlobEntry, BlobStore, DType};
pub use codec::{
    decode_frame, encode_blob, encode_blob_ack, encode_blob_request, encode_sync, BlobHeader,
    CodecError, Frame, FrameType, BLOB_HEADER_LEN,
};
pub use ids::{mint_object_id, ObjectId};
pub use scene::{Author, BlobRef, ObjectSnapshot, PropValue, Scene, SceneError};
