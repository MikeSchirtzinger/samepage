//! The honesty bench: what does it cost to apply the SAME logical change
//! ("move N objects") arriving in each wire representation?
//!
//! (a) RFC 6902 JSON Patch — the current `StateDeltaEvent` representation:
//!     parse the patch text, walk paths, mutate a `serde_json::Value` scene.
//! (b) yrs binary update — `Update::decode_v1` + `apply_update` on a replica.
//! (c) blob frame — header decode + one aligned memcpy (the GPU staging
//!     copy), i.e. the bulk-geometry fast path.
//!
//! Per the design notes' appendix: wire SIZE is not the claim (gzip eats
//! JSON's redundancy) — parse/alloc cost is. Each routine starts from a
//! fresh receiver so repeated iterations don't no-op.

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use std::time::Duration;

use ag_ui_canvas::blob::AlignedBytes;
use ag_ui_canvas::codec::{decode_frame, encode_blob, BlobHeader, Frame};
use ag_ui_canvas::scene::{Author, PropValue, Scene};
use ag_ui_canvas::{DType, ObjectId};

const SIZES: &[usize] = &[10, 100, 1000, 10_000];

struct Fixture {
    /// Full base state (N objects) as a v1 update — receivers rehydrate from
    /// this in setup.
    base_update: Vec<u8>,
    /// The same N-object move as RFC 6902 JSON Patch text.
    patch_text: String,
    /// Base scene as a JSON document the patch applies to.
    json_doc: serde_json::Value,
    /// The same move as one yrs v1 update.
    yrs_update: Vec<u8>,
    /// N*2 f32 positions as an encoded blob frame (aligned receive buffer).
    blob_frame: AlignedBytes,
}

fn build_fixture(n: usize) -> Fixture {
    // Base scene: N objects with the demo's property set.
    let mut base = Scene::new();
    let ids: Vec<ObjectId> = (0..n)
        .map(|i| {
            let id = base.create_object("disc", Author::Human).unwrap();
            base.set_props(
                &id,
                &[
                    ("x", PropValue::Num(i as f64)),
                    ("y", PropValue::Num(i as f64 * 2.0)),
                    ("scale", PropValue::Num(10.0)),
                    ("color", PropValue::Num(0xFFFFFFFFu32 as f64)),
                ],
            )
            .unwrap();
            id
        })
        .collect();
    let base_update = base
        .encode_full()
        .expect("benchmark base scene should encode");
    let base_sv = base
        .state_vector()
        .expect("benchmark base state vector should be readable");

    // (a) JSON document + patch text.
    let mut objects = serde_json::Map::new();
    for (i, id) in ids.iter().enumerate() {
        objects.insert(
            id.to_string(),
            serde_json::json!({
                "kind": "disc",
                "x": i as f64,
                "y": i as f64 * 2.0,
                "scale": 10.0,
                "color": 0xFFFFFFFFu32,
            }),
        );
    }
    let json_doc = serde_json::json!({ "objects": objects });
    let ops: Vec<serde_json::Value> = ids
        .iter()
        .enumerate()
        .flat_map(|(i, id)| {
            [
                serde_json::json!({
                    "op": "replace",
                    "path": format!("/objects/{id}/x"),
                    "value": i as f64 + 500.0,
                }),
                serde_json::json!({
                    "op": "replace",
                    "path": format!("/objects/{id}/y"),
                    "value": i as f64 - 500.0,
                }),
            ]
        })
        .collect();
    let patch_text = serde_json::to_string(&ops).unwrap();

    // (b) The identical move as a yrs update: mutate a copy of the base,
    // diff against the base state vector.
    let source = Scene::from_state(&base_update).unwrap();
    for (i, id) in ids.iter().enumerate() {
        source
            .set_props(
                id,
                &[
                    ("x", PropValue::Num(i as f64 + 500.0)),
                    ("y", PropValue::Num(i as f64 - 500.0)),
                ],
            )
            .unwrap();
    }
    let yrs_update = source.encode_diff(&base_sv).unwrap();

    // (c) The move as bulk geometry: N (x, y) f32 pairs in a blob frame,
    // landed in an aligned receive buffer.
    let positions: Vec<f32> = (0..n)
        .flat_map(|i| [i as f32 + 500.0, i as f32 - 500.0])
        .collect();
    let frame = encode_blob(
        &BlobHeader {
            dtype: DType::F32,
            ndim: 2,
            blob_id: 1,
            generation: 1,
            element_count: (n * 2) as u32,
            shape: [n as u32, 2],
        },
        bytemuck::cast_slice(&positions),
    )
    .expect("benchmark blob should encode");
    let blob_frame = AlignedBytes::from_bytes(frame.as_bytes());

    Fixture {
        base_update,
        patch_text,
        json_doc,
        yrs_update,
        blob_frame,
    }
}

fn bench_apply_cost(c: &mut Criterion) {
    for &n in SIZES {
        let fx = build_fixture(n);
        println!(
            "[wire bytes @ n={n}] json_patch={} yrs_update={} blob_frame={}",
            fx.patch_text.len(),
            fx.yrs_update.len(),
            fx.blob_frame.len(),
        );

        let mut group = c.benchmark_group(format!("apply_move_{n}_objects"));
        group
            .warm_up_time(Duration::from_millis(500))
            .measurement_time(Duration::from_secs(2))
            .sample_size(50);

        // (a) JSON Patch: parse text + apply to the document.
        group.bench_function("json_patch", |b| {
            b.iter_batched(
                || fx.json_doc.clone(),
                |mut doc| {
                    let patch: json_patch::Patch = serde_json::from_str(&fx.patch_text).unwrap();
                    json_patch::patch(&mut doc, &patch).unwrap();
                    doc
                },
                BatchSize::SmallInput,
            )
        });

        // (b) yrs: decode + apply the binary update to a synced replica.
        group.bench_function("yrs_update", |b| {
            b.iter_batched(
                || Scene::from_state(&fx.base_update).unwrap(),
                |receiver| {
                    receiver.apply_update(&fx.yrs_update).unwrap();
                    receiver
                },
                BatchSize::SmallInput,
            )
        });

        // (c) blob: header decode + the one staging memcpy.
        group.bench_function("blob_memcpy", |b| {
            let payload_len = fx.blob_frame.len() - ag_ui_canvas::BLOB_HEADER_LEN;
            b.iter_batched(
                || AlignedBytes::zeroed(payload_len),
                |mut staging| {
                    let Frame::Blob { payload, .. } =
                        decode_frame(fx.blob_frame.as_bytes()).unwrap()
                    else {
                        unreachable!()
                    };
                    staging.as_bytes_mut().copy_from_slice(payload);
                    staging
                },
                BatchSize::SmallInput,
            )
        });

        group.finish();
    }
}

criterion_group!(benches, bench_apply_cost);
criterion_main!(benches);
