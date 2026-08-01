//! CRDT scene graph over a yrs Doc.
//!
//! Layout: one root map `"objects"` keyed by [`ObjectId`]; each object is a
//! nested YMap of flat scalar properties. Flat keys make conflict resolution
//! per-property last-write-wins, so a human dragging `x`/`y` and an agent
//! setting `color` on the SAME object concurrently both survive the merge.
//! Concurrent writes to the same key resolve deterministically (every replica
//! agrees on the winner) — callers should treat property writes as LWW, not
//! mergeable.
//!
//! Bulk geometry stays OUT of the CRDT: an object carries a [`BlobRef`]
//! (id + generation + count + dtype) naming a payload in the blob channel.
//!
//! ## wasm caution
//!
//! All transactions go through `try_transact`/`try_transact_mut`: the blocking
//! variants panic on wasm under contention. Single-threaded wasm only contends
//! when re-entering (e.g. mutating from inside an observer callback) — which
//! this API surfaces as `SceneError::Txn` instead of a panic. Never mutate the
//! scene from inside an observer.

use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{
    Any, Doc, Map, MapPrelim, MapRef, Out, ReadTxn, StateVector, Subscription, Transact, Update,
};

use crate::blob::DType;
use crate::codec::BlobHeader;
use crate::ids::{mint_object_id, ObjectId};

/// Who authored an object / a mutation. Stored as plain strings so the
/// vocabulary can grow (e.g. named agent runs) without schema changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Author {
    Human,
    Agent,
    Named(String),
}

impl Author {
    pub fn as_str(&self) -> &str {
        match self {
            Author::Human => "human",
            Author::Agent => "agent",
            Author::Named(s) => s,
        }
    }
}

/// Scalar property value. Colors are packed RGBA u32s stored as numbers
/// (lossless: u32 < 2^53).
#[derive(Debug, Clone, PartialEq)]
pub enum PropValue {
    Num(f64),
    Str(String),
    Bool(bool),
}

impl PropValue {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            PropValue::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            PropValue::Str(s) => Some(s),
            _ => None,
        }
    }

    fn to_any(&self) -> Any {
        match self {
            PropValue::Num(n) => Any::Number(*n),
            PropValue::Str(s) => Any::from(s.as_str()),
            PropValue::Bool(b) => Any::Bool(*b),
        }
    }

    fn from_any(any: &Any) -> Option<PropValue> {
        match any {
            Any::Number(n) => Some(PropValue::Num(*n)),
            Any::BigInt(n) => Some(PropValue::Num(*n as f64)),
            Any::String(s) => Some(PropValue::Str(s.to_string())),
            Any::Bool(b) => Some(PropValue::Bool(*b)),
            _ => None,
        }
    }
}

/// Pointer from a scene object to its bulk-geometry blob. `count`/`dtype` let
/// the renderer validate (and pre-allocate for) an incoming blob frame before
/// it arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobRef {
    pub blob_id: u64,
    pub generation: u32,
    pub count: u32,
    pub dtype: DType,
}

impl BlobRef {
    /// Whether an inbound frame belongs to this live geometry reference.
    /// `generation` is a stream epoch/minimum: high-rate producers may advance
    /// frame generations without rewriting the CRDT on every tick, while the
    /// renderer rejects resident generation regressions.
    pub fn accepts_header(&self, header: &BlobHeader) -> bool {
        header.dtype == DType::F32
            && header.ndim == 2
            && header.shape[1] == 2
            && header.shape[0].checked_mul(header.shape[1]) == Some(header.element_count)
            && self.blob_id == header.blob_id
            && header.generation >= self.generation
            && self.count == header.element_count
            && self.dtype == header.dtype
    }
}

/// Plain-data snapshot of one object, for the renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectSnapshot {
    pub id: ObjectId,
    pub kind: String,
    pub owner: String,
    pub x: f32,
    pub y: f32,
    /// Second endpoint for `line` objects (the renderer draws an oriented quad
    /// from (x,y) to (x2,y2)). Defaults to (x,y) for everything else.
    pub x2: f32,
    pub y2: f32,
    pub scale: f32,
    /// Packed RGBA (0xRRGGBBAA).
    pub color: u32,
    pub z_index: f64,
    pub blob_ref: Option<BlobRef>,
    /// Text content for `label` objects (the on-canvas text the renderer draws
    /// via the glyph atlas). `None` for non-text objects.
    pub text: Option<String>,
}

/// Resolve the display color for an authorized inbound geometry frame.
/// Unknown ids, non-particle objects, and mismatched metadata are rejected so
/// delayed frames cannot resurrect geometry after its scene object is gone.
pub fn authorized_blob_color(objects: &[ObjectSnapshot], header: &BlobHeader) -> Option<u32> {
    objects
        .iter()
        .find(|object| {
            object.kind == "particles"
                && object
                    .blob_ref
                    .is_some_and(|blob| blob.accepts_header(header))
        })
        .map(|object| object.color)
}

#[derive(Debug, thiserror::Error)]
pub enum SceneError {
    #[error("object not found: {0}")]
    NotFound(String),
    #[error("transaction unavailable (re-entrant mutation?): {0}")]
    Txn(String),
    #[error("update decode failed: {0}")]
    Decode(String),
    #[error("update apply failed: {0}")]
    Apply(String),
    #[error("invalid scene data: {0}")]
    InvalidData(String),
}

// Flat property keys for the blob reference. blob_id is stored as a hex
// string: u64 doesn't survive an f64 round-trip above 2^53.
const K_BLOB_ID: &str = "blob_id";
const K_BLOB_GEN: &str = "blob_gen";
const K_BLOB_COUNT: &str = "blob_count";
const K_BLOB_DTYPE: &str = "blob_dtype";

/// The shared canvas state: a yrs Doc plus typed accessors.
pub struct Scene {
    doc: Doc,
    objects: MapRef,
    local_seq: u32,
}

impl Default for Scene {
    fn default() -> Self {
        Self::new()
    }
}

impl Scene {
    pub fn new() -> Self {
        let doc = Doc::new();
        let objects = doc.get_or_insert_map("objects");
        Self {
            doc,
            objects,
            local_seq: 0,
        }
    }

    /// Rehydrate from a full-state v1 update (snapshot load / reconnect).
    pub fn from_state(update_v1: &[u8]) -> Result<Self, SceneError> {
        let scene = Self::new();
        scene.apply_update(update_v1)?;
        Ok(scene)
    }

    /// The underlying Doc handle (shared state — clones observe the same doc).
    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    pub fn client_id(&self) -> u64 {
        // 53-bit yjs-compatible id; `get()` strips yrs's internal tag bits.
        self.doc.client_id().get()
    }

    fn read_txn(&self) -> Result<yrs::Transaction<'_>, SceneError> {
        self.doc
            .try_transact()
            .map_err(|e| SceneError::Txn(e.to_string()))
    }

    fn write_txn(&self) -> Result<yrs::TransactionMut<'_>, SceneError> {
        self.doc
            .try_transact_mut()
            .map_err(|e| SceneError::Txn(e.to_string()))
    }

    fn object_ref(&self, txn: &impl ReadTxn, id: &ObjectId) -> Result<MapRef, SceneError> {
        match self.objects.get(txn, id.as_str()) {
            Some(Out::YMap(map)) => Ok(map),
            _ => Err(SceneError::NotFound(id.to_string())),
        }
    }

    /// Create an object with a freshly minted collision-free id.
    pub fn create_object(&mut self, kind: &str, owner: Author) -> Result<ObjectId, SceneError> {
        self.create_object_with_props(kind, owner, &[])
    }

    /// Create and fully configure one object in a single Yrs transaction.
    /// Observers therefore never receive a kind-only shell before its initial
    /// position/style properties arrive.
    pub fn create_object_with_props(
        &mut self,
        kind: &str,
        owner: Author,
        props: &[(&str, PropValue)],
    ) -> Result<ObjectId, SceneError> {
        self.create_object_configured(kind, owner, props, None)
    }

    /// Create and configure a bulk-geometry object, including its blob
    /// reference, in one Yrs transaction.
    pub fn create_object_with_props_and_blob(
        &mut self,
        kind: &str,
        owner: Author,
        props: &[(&str, PropValue)],
        blob: BlobRef,
    ) -> Result<ObjectId, SceneError> {
        self.create_object_configured(kind, owner, props, Some(blob))
    }

    fn create_object_configured(
        &mut self,
        kind: &str,
        owner: Author,
        props: &[(&str, PropValue)],
        blob: Option<BlobRef>,
    ) -> Result<ObjectId, SceneError> {
        Self::validate_props(props)?;
        if props
            .iter()
            .any(|(key, _)| matches!(*key, "kind" | "owner" | "created_by"))
        {
            return Err(SceneError::InvalidData(
                "kind and ownership fields are supplied by create_object".into(),
            ));
        }
        if blob.is_some()
            && props.iter().any(|(key, _)| {
                matches!(*key, K_BLOB_ID | K_BLOB_GEN | K_BLOB_COUNT | K_BLOB_DTYPE)
            })
        {
            return Err(SceneError::InvalidData(
                "blob properties must be supplied only through the blob argument".into(),
            ));
        }
        let id = mint_object_id(self.client_id(), self.local_seq);
        {
            let mut txn = self.write_txn()?;
            let obj = self
                .objects
                .insert(&mut txn, id.as_str(), MapPrelim::default());
            obj.insert(&mut txn, "kind", Any::from(kind));
            obj.insert(&mut txn, "owner", Any::from(owner.as_str()));
            obj.insert(&mut txn, "created_by", Any::from(owner.as_str()));
            for (key, value) in props {
                obj.insert(&mut txn, *key, value.to_any());
            }
            if let Some(blob) = blob {
                obj.insert(
                    &mut txn,
                    K_BLOB_ID,
                    Any::from(format!("{:016x}", blob.blob_id)),
                );
                obj.insert(&mut txn, K_BLOB_GEN, Any::from(blob.generation as f64));
                obj.insert(&mut txn, K_BLOB_COUNT, Any::from(blob.count as f64));
                obj.insert(&mut txn, K_BLOB_DTYPE, Any::from(blob.dtype.tag() as f64));
            }
        }
        self.local_seq += 1;
        Ok(id)
    }

    /// Set one scalar property (per-key LWW on concurrent writes).
    pub fn set_prop(&self, id: &ObjectId, key: &str, value: PropValue) -> Result<(), SceneError> {
        self.set_props(id, &[(key, value)])
    }

    /// Set several properties in ONE transaction (= one outbound update).
    /// Use this for drag moves: `[("x", ..), ("y", ..)]`.
    pub fn set_props(&self, id: &ObjectId, props: &[(&str, PropValue)]) -> Result<(), SceneError> {
        Self::validate_props(props)?;
        let mut txn = self.write_txn()?;
        let obj = self.object_ref(&txn, id)?;
        for (key, value) in props {
            obj.insert(&mut txn, *key, value.to_any());
        }
        Ok(())
    }

    fn validate_props(props: &[(&str, PropValue)]) -> Result<(), SceneError> {
        let blob_keys = [K_BLOB_ID, K_BLOB_GEN, K_BLOB_COUNT, K_BLOB_DTYPE];
        let referenced_blob_keys: std::collections::BTreeSet<&str> = props
            .iter()
            .map(|(key, _)| *key)
            .filter(|key| blob_keys.contains(key))
            .collect();
        if !referenced_blob_keys.is_empty()
            && (referenced_blob_keys.len() != blob_keys.len()
                || props
                    .iter()
                    .filter(|(key, _)| blob_keys.contains(key))
                    .count()
                    != blob_keys.len())
        {
            return Err(SceneError::InvalidData(
                "blob reference fields must be written together with set_blob_ref".into(),
            ));
        }

        for (key, value) in props {
            let numeric_field = matches!(
                *key,
                "x" | "y"
                    | "x2"
                    | "y2"
                    | "scale"
                    | "z_index"
                    | "color"
                    | K_BLOB_GEN
                    | K_BLOB_COUNT
                    | K_BLOB_DTYPE
            );
            let string_field = matches!(*key, "kind" | "owner" | "created_by" | "text" | K_BLOB_ID);
            if numeric_field && !matches!(value, PropValue::Num(_)) {
                return Err(SceneError::InvalidData(format!(
                    "field `{key}` must be numeric"
                )));
            }
            if string_field && !matches!(value, PropValue::Str(_)) {
                return Err(SceneError::InvalidData(format!(
                    "field `{key}` must be a string"
                )));
            }
            match value {
                PropValue::Num(number) => {
                    if !number.is_finite() {
                        return Err(SceneError::InvalidData(format!(
                            "field `{key}` must be finite"
                        )));
                    }
                    if matches!(*key, "x" | "y" | "x2" | "y2" | "scale")
                        && (*number < -f64::from(f32::MAX) || *number > f64::from(f32::MAX))
                    {
                        return Err(SceneError::InvalidData(format!(
                            "field `{key}` must fit in f32"
                        )));
                    }
                    if *key == "color"
                        && (number.fract() != 0.0 || *number < 0.0 || *number > f64::from(u32::MAX))
                    {
                        return Err(SceneError::InvalidData(
                            "field `color` must be an exact u32".into(),
                        ));
                    }
                    if matches!(*key, K_BLOB_GEN | K_BLOB_COUNT | K_BLOB_DTYPE)
                        && (number.fract() != 0.0 || *number < 0.0 || *number > f64::from(u32::MAX))
                    {
                        return Err(SceneError::InvalidData(format!(
                            "blob field `{key}` must be an exact u32"
                        )));
                    }
                    if *key == K_BLOB_DTYPE
                        && u8::try_from(*number as u32)
                            .ok()
                            .and_then(DType::from_tag)
                            .is_none()
                    {
                        return Err(SceneError::InvalidData(format!(
                            "blob dtype tag {number} is unsupported"
                        )));
                    }
                }
                PropValue::Str(value) if *key == K_BLOB_ID => {
                    if value.len() != 16 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                        return Err(SceneError::InvalidData(
                            "blob id must be a 16-digit hexadecimal string".into(),
                        ));
                    }
                }
                PropValue::Str(_) | PropValue::Bool(_) => {
                    if blob_keys.contains(key) {
                        return Err(SceneError::InvalidData(format!(
                            "blob field `{key}` has the wrong type"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn get_prop(&self, id: &ObjectId, key: &str) -> Result<Option<PropValue>, SceneError> {
        let txn = self.read_txn()?;
        let Ok(obj) = self.object_ref(&txn, id) else {
            return Ok(None);
        };
        Ok(match obj.get(&txn, key) {
            Some(Out::Any(any)) => PropValue::from_any(&any),
            _ => None,
        })
    }

    /// Point this object at blob `(id, generation)`. Done in one transaction
    /// so a generation bump and its metadata land atomically.
    pub fn set_blob_ref(&self, id: &ObjectId, blob: BlobRef) -> Result<(), SceneError> {
        self.set_props(
            id,
            &[
                (K_BLOB_ID, PropValue::Str(format!("{:016x}", blob.blob_id))),
                (K_BLOB_GEN, PropValue::Num(blob.generation as f64)),
                (K_BLOB_COUNT, PropValue::Num(blob.count as f64)),
                (K_BLOB_DTYPE, PropValue::Num(blob.dtype.tag() as f64)),
            ],
        )
    }

    pub fn get_blob_ref(&self, id: &ObjectId) -> Result<Option<BlobRef>, SceneError> {
        let txn = self.read_txn()?;
        let Ok(obj) = self.object_ref(&txn, id) else {
            return Ok(None);
        };
        Self::blob_ref_of(&txn, &obj, id.as_str())
    }

    fn blob_ref_of(
        txn: &impl ReadTxn,
        obj: &MapRef,
        object_id: &str,
    ) -> Result<Option<BlobRef>, SceneError> {
        let values =
            [K_BLOB_ID, K_BLOB_GEN, K_BLOB_COUNT, K_BLOB_DTYPE].map(|key| obj.get(txn, key));
        if values.iter().all(Option::is_none) {
            return Ok(None);
        }
        if values.iter().any(Option::is_none) {
            return Err(SceneError::InvalidData(format!(
                "object `{object_id}` has a partial blob reference"
            )));
        }
        let get_any = |index: usize, key: &str| match values.get(index).and_then(Option::as_ref) {
            Some(Out::Any(any)) => Ok(any),
            _ => Err(SceneError::InvalidData(format!(
                "object `{object_id}` blob field `{key}` has the wrong type"
            ))),
        };
        let blob_id = match get_any(0, K_BLOB_ID)? {
            Any::String(value)
                if value.len() == 16 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
            {
                u64::from_str_radix(value, 16).map_err(|error| {
                    SceneError::InvalidData(format!(
                        "object `{object_id}` blob id is invalid: {error}"
                    ))
                })?
            }
            _ => {
                return Err(SceneError::InvalidData(format!(
                    "object `{object_id}` blob id must be a 16-digit hexadecimal string"
                )))
            }
        };
        let exact_u32 = |index: usize, key: &str| -> Result<u32, SceneError> {
            let value = match get_any(index, key)? {
                Any::Number(number) => *number,
                Any::BigInt(number) => u32::try_from(*number).map(f64::from).map_err(|_| {
                    SceneError::InvalidData(format!(
                        "object `{object_id}` blob field `{key}` is outside u32"
                    ))
                })?,
                _ => {
                    return Err(SceneError::InvalidData(format!(
                        "object `{object_id}` blob field `{key}` must be an exact u32"
                    )))
                }
            };
            (value.is_finite()
                && value.fract() == 0.0
                && value >= 0.0
                && value <= f64::from(u32::MAX))
            .then_some(value as u32)
            .ok_or_else(|| {
                SceneError::InvalidData(format!(
                    "object `{object_id}` blob field `{key}` must be an exact u32"
                ))
            })
        };
        let dtype_tag = u8::try_from(exact_u32(3, K_BLOB_DTYPE)?).map_err(|_| {
            SceneError::InvalidData(format!("object `{object_id}` blob dtype tag is outside u8"))
        })?;
        let dtype = DType::from_tag(dtype_tag).ok_or_else(|| {
            SceneError::InvalidData(format!(
                "object `{object_id}` blob dtype tag {dtype_tag} is unsupported"
            ))
        })?;
        Ok(Some(BlobRef {
            blob_id,
            generation: exact_u32(1, K_BLOB_GEN)?,
            count: exact_u32(2, K_BLOB_COUNT)?,
            dtype,
        }))
    }

    pub fn delete_object(&self, id: &ObjectId) -> Result<(), SceneError> {
        let mut txn = self.write_txn()?;
        self.objects
            .remove(&mut txn, id.as_str())
            .ok_or_else(|| SceneError::NotFound(id.to_string()))?;
        Ok(())
    }

    /// Delete every object in one CRDT transaction and return the deleted ids.
    ///
    /// This reads the authoritative Yrs map rather than any process-local
    /// object registry, so it also clears objects learned from a reconnecting
    /// replica after this process started.
    pub fn clear_objects(&self) -> Result<Vec<ObjectId>, SceneError> {
        let mut txn = self.write_txn()?;
        let ids: Vec<ObjectId> = self.objects.keys(&txn).map(ObjectId::from).collect();
        for id in &ids {
            self.objects.remove(&mut txn, id.as_str());
        }
        Ok(ids)
    }

    /// All object ids, sorted — yrs map iteration order is hash-based and
    /// differs between replicas, so we normalize for deterministic output.
    pub fn object_ids(&self) -> Result<Vec<ObjectId>, SceneError> {
        let txn = self.read_txn()?;
        let mut ids: Vec<ObjectId> = self.objects.keys(&txn).map(ObjectId::from).collect();
        ids.sort();
        Ok(ids)
    }

    pub fn len(&self) -> Result<usize, SceneError> {
        let txn = self.read_txn()?;
        Ok(self.objects.len(&txn) as usize)
    }

    pub fn is_empty(&self) -> Result<bool, SceneError> {
        Ok(self.len()? == 0)
    }

    /// Plain-data snapshot of every object, defaults filled in — the
    /// renderer's input. Sorted by (z_index, id) for stable draw order.
    pub fn snapshot(&self) -> Result<Vec<ObjectSnapshot>, SceneError> {
        let txn = self.read_txn()?;
        let mut out: Vec<ObjectSnapshot> = self
            .objects
            .iter(&txn)
            .map(|(key, value)| {
                let obj = match value {
                    Out::YMap(map) => map,
                    _ => {
                        return Err(SceneError::InvalidData(format!(
                            "object `{key}` is not a map"
                        )))
                    }
                };
                let invalid_field = |field: &str, expected: &str| {
                    SceneError::InvalidData(format!(
                        "object `{key}` field `{field}` must be {expected}"
                    ))
                };
                let str_of = |field: &str| -> Result<Option<String>, SceneError> {
                    match obj.get(&txn, field) {
                        None => Ok(None),
                        Some(Out::Any(Any::String(value))) => Ok(Some(value.to_string())),
                        Some(_) => Err(invalid_field(field, "a string")),
                    }
                };
                let num_of = |field: &str| -> Result<Option<f64>, SceneError> {
                    let value = match obj.get(&txn, field) {
                        None => return Ok(None),
                        Some(Out::Any(Any::Number(value))) => value,
                        Some(Out::Any(Any::BigInt(value))) => value as f64,
                        Some(_) => return Err(invalid_field(field, "a finite number")),
                    };
                    value
                        .is_finite()
                        .then_some(Some(value))
                        .ok_or_else(|| invalid_field(field, "a finite number"))
                };
                let f32_of = |field: &str| -> Result<Option<f32>, SceneError> {
                    let Some(value) = num_of(field)? else {
                        return Ok(None);
                    };
                    (value >= -f64::from(f32::MAX) && value <= f64::from(f32::MAX))
                        .then_some(Some(value as f32))
                        .ok_or_else(|| invalid_field(field, "a finite f32-range number"))
                };
                let color_of = || -> Result<Option<u32>, SceneError> {
                    let Some(value) = num_of("color")? else {
                        return Ok(None);
                    };
                    (value.fract() == 0.0 && value >= 0.0 && value <= f64::from(u32::MAX))
                        .then_some(Some(value as u32))
                        .ok_or_else(|| invalid_field("color", "an exact u32"))
                };
                let x = f32_of("x")?.unwrap_or(0.0);
                let y = f32_of("y")?.unwrap_or(0.0);
                Ok(ObjectSnapshot {
                    id: ObjectId::from(key),
                    kind: str_of("kind")?.unwrap_or_else(|| "unknown".to_string()),
                    owner: str_of("owner")?.unwrap_or_default(),
                    x,
                    y,
                    x2: f32_of("x2")?.unwrap_or(x),
                    y2: f32_of("y2")?.unwrap_or(y),
                    scale: f32_of("scale")?.unwrap_or(1.0),
                    color: color_of()?.unwrap_or(0xFFFFFFFF),
                    z_index: num_of("z_index")?.unwrap_or(0.0),
                    blob_ref: match Self::blob_ref_of(&txn, &obj, key) {
                        Ok(blob_ref) => blob_ref,
                        Err(error) => return Err(error),
                    },
                    text: str_of("text")?,
                })
            })
            .collect::<Result<Vec<_>, SceneError>>()?;
        // `total_cmp` is a stable, transitive ordering for the validated
        // finite z-index values and keeps replica draw order deterministic.
        out.sort_by(|a, b| {
            a.z_index
                .total_cmp(&b.z_index)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(out)
    }

    // ── sync plumbing ────────────────────────────────────────────────────

    pub fn state_vector(&self) -> Result<StateVector, SceneError> {
        let txn = self.read_txn()?;
        Ok(txn.state_vector())
    }

    pub fn state_vector_v1(&self) -> Result<Vec<u8>, SceneError> {
        Ok(self.state_vector()?.encode_v1())
    }

    /// Everything the remote (described by `remote_sv`) is missing, as a v1
    /// update — the differential catch-up used on reconnect.
    pub fn encode_diff(&self, remote_sv: &StateVector) -> Result<Vec<u8>, SceneError> {
        let txn = self.read_txn()?;
        Ok(txn.encode_state_as_update_v1(remote_sv))
    }

    pub fn encode_diff_v1(&self, remote_sv_v1: &[u8]) -> Result<Vec<u8>, SceneError> {
        let sv =
            StateVector::decode_v1(remote_sv_v1).map_err(|e| SceneError::Decode(e.to_string()))?;
        self.encode_diff(&sv)
    }

    /// Full state as one v1 update (snapshot/compaction payload).
    pub fn encode_full(&self) -> Result<Vec<u8>, SceneError> {
        self.encode_diff(&StateVector::default())
    }

    pub fn apply_update(&self, update_v1: &[u8]) -> Result<(), SceneError> {
        let update = Update::decode_v1(update_v1).map_err(|e| SceneError::Decode(e.to_string()))?;
        let mut txn = self.write_txn()?;
        txn.apply_update(update)
            .map_err(|e| SceneError::Apply(e.to_string()))
    }

    /// Fire `cb` with the v1 update bytes of every committed local-or-applied
    /// transaction — the bridge to "put it on the wire". Keep the returned
    /// Subscription alive; dropping it unsubscribes. Do NOT mutate the scene
    /// from inside the callback (re-entrant transaction).
    ///
    /// Native requires `Send + Sync` (the server shares the Scene across
    /// tokio threads); wasm is single-threaded, so browser callbacks may
    /// capture `Rc`/JS handles.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn on_update(
        &self,
        cb: impl Fn(&[u8]) + Send + Sync + 'static,
    ) -> Result<Subscription, SceneError> {
        self.doc
            .observe_update_v1(move |_txn, event| cb(&event.update))
            .map_err(|e| SceneError::Txn(e.to_string()))
    }

    /// See the native variant; wasm drops the `Send + Sync` bounds.
    #[cfg(target_arch = "wasm32")]
    pub fn on_update(&self, cb: impl Fn(&[u8]) + 'static) -> Result<Subscription, SceneError> {
        self.doc
            .observe_update_v1(move |_txn, event| cb(&event.update))
            .map_err(|e| SceneError::Txn(e.to_string()))
    }

    /// Long-session hygiene: full-state snapshot for rebasing into a fresh
    /// Scene (drops tombstone/causal garbage). Coordinated compaction — all
    /// peers rebase from the same snapshot — is future work; see notes §2.
    pub fn compact_snapshot(&self) -> Result<Vec<u8>, SceneError> {
        self.encode_full()
    }
}
