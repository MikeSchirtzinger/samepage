//! Bulk-geometry blob storage.
//!
//! Blobs (point clouds, later tensors) deliberately live OUTSIDE the CRDT —
//! per-element causal metadata on a 100k-point cloud would swamp the actual
//! data. A blob is immutable per `(blob_id, generation)`; rewriting means
//! bumping the generation, never merging bytes. The CRDT scene object carries
//! a [`crate::scene::BlobRef`] naming the blob it renders.
//!
//! [`AlignedBytes`] keeps every buffer 8-byte aligned so an f32/u32 payload at
//! a multiple-of-8 offset can be reinterpreted in place (`bytemuck`) with no
//! repack — the receive path MUST copy wire bytes into one of these, not a
//! plain `Vec<u8>` (alignment 1), or in-place casts become invalid.

use std::collections::HashMap;

/// Element type of a blob payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DType {
    F32,
    F16,
    U8,
    U32,
    I32,
}

impl DType {
    /// Size of one element in bytes.
    pub const fn size(self) -> usize {
        match self {
            DType::F32 | DType::U32 | DType::I32 => 4,
            DType::F16 => 2,
            DType::U8 => 1,
        }
    }

    /// Required pointer alignment for in-place reinterpretation.
    pub const fn align(self) -> usize {
        self.size()
    }

    /// Wire tag (see codec blob header).
    pub const fn tag(self) -> u8 {
        match self {
            DType::F32 => 1,
            DType::F16 => 2,
            DType::U8 => 3,
            DType::U32 => 4,
            DType::I32 => 5,
        }
    }

    pub fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(DType::F32),
            2 => Some(DType::F16),
            3 => Some(DType::U8),
            4 => Some(DType::U32),
            5 => Some(DType::I32),
            _ => None,
        }
    }
}

/// Byte buffer whose base address is always 8-byte aligned (backed by `u64`
/// words). Guarantees that any multiple-of-8 offset into the buffer is itself
/// 8-byte aligned in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlignedBytes {
    words: Vec<u64>,
    len: usize,
}

impl AlignedBytes {
    pub fn zeroed(len: usize) -> Self {
        Self {
            words: vec![0u64; len.div_ceil(8)],
            len,
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut out = Self::zeroed(bytes.len());
        out.as_bytes_mut().copy_from_slice(bytes);
        out
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_bytes(&self) -> &[u8] {
        let storage = bytemuck::cast_slice(&self.words);
        // `words` and `len` are private. Every constructor allocates
        // `len.div_ceil(8)` words, and cloning preserves that invariant.
        storage.split_at(self.len).0
    }

    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        let storage = bytemuck::cast_slice_mut(&mut self.words);
        // The same private representation invariant documented in `as_bytes`
        // guarantees the logical byte length fits this storage.
        storage.split_at_mut(self.len).0
    }

    /// Reinterpret the buffer as `f32`s in place. Alignment is guaranteed by
    /// construction; an invalid byte length is returned as a cast error.
    pub fn as_f32(&self) -> Result<&[f32], bytemuck::PodCastError> {
        bytemuck::try_cast_slice(self.as_bytes())
    }
}

/// One stored blob: payload plus the metadata the renderer validates against
/// the CRDT's `BlobRef` before upload.
#[derive(Debug, Clone)]
pub struct BlobEntry {
    pub dtype: DType,
    /// `[dim0, dim1]`, e.g. `[N, 3]` for an Nx3 point cloud. `0` = unused.
    pub shape: [u32; 2],
    pub generation: u32,
    pub bytes: AlignedBytes,
}

/// Generation-checked blob map. Single-writer per blob id (the creator of the
/// owning scene object); enforcement lives at the server boundary, not here.
#[derive(Debug, Default)]
pub struct BlobStore {
    entries: HashMap<u64, BlobEntry>,
}

impl BlobStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store `entry` unless we already hold the same or a newer generation.
    /// Returns whether the entry was stored.
    pub fn insert(&mut self, blob_id: u64, entry: BlobEntry) -> bool {
        match self.entries.get(&blob_id) {
            Some(existing) if existing.generation >= entry.generation => false,
            _ => {
                self.entries.insert(blob_id, entry);
                true
            }
        }
    }

    pub fn get(&self, blob_id: u64) -> Option<&BlobEntry> {
        self.entries.get(&blob_id)
    }

    pub fn current_generation(&self, blob_id: u64) -> Option<u32> {
        self.entries.get(&blob_id).map(|e| e.generation)
    }

    pub fn remove(&mut self, blob_id: u64) -> Option<BlobEntry> {
        self.entries.remove(&blob_id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_bytes_base_is_8_aligned() {
        for len in [0usize, 1, 7, 8, 9, 40, 4001] {
            let buf = AlignedBytes::zeroed(len);
            assert_eq!(buf.as_bytes().as_ptr() as usize % 8, 0, "len={len}");
            assert_eq!(buf.len(), len);
        }
    }

    #[test]
    fn aligned_bytes_f32_roundtrip() {
        let floats = [1.0f32, -2.5, 3.25, 0.0];
        let buf = AlignedBytes::from_bytes(bytemuck::cast_slice(&floats));
        assert_eq!(
            buf.as_f32().expect("aligned f32 bytes should cast"),
            &floats
        );
    }

    #[test]
    fn blob_store_rejects_stale_generations() {
        let mut store = BlobStore::new();
        let entry = |generation| BlobEntry {
            dtype: DType::F32,
            shape: [2, 2],
            generation,
            bytes: AlignedBytes::zeroed(16),
        };
        assert!(store.insert(7, entry(1)));
        assert!(!store.insert(7, entry(1)), "same generation rejected");
        assert!(!store.insert(7, entry(0)), "older generation rejected");
        assert!(store.insert(7, entry(2)), "newer generation accepted");
        assert_eq!(store.current_generation(7), Some(2));
    }
}
