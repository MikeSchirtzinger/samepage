//! CRDT convergence tests: two Scenes, concurrent edits, exchanged updates,
//! asserted agreement. These prove the co-edit semantics before any
//! networking exists.

use ag_ui_canvas::scene::{Author, BlobRef, PropValue, Scene};
use ag_ui_canvas::sync::{greeting, handle_payload, update_message};
use ag_ui_canvas::DType;
use std::sync::{Arc, Mutex};
use yrs::{Map, Out, ReadTxn, Transact};

/// Bidirectional full sync via state-vector diffs.
fn sync_both(a: &Scene, b: &Scene) {
    let to_b = a
        .encode_diff(&b.state_vector().expect("read replica B state vector"))
        .unwrap();
    let to_a = b
        .encode_diff(&a.state_vector().expect("read replica A state vector"))
        .unwrap();
    b.apply_update(&to_b).unwrap();
    a.apply_update(&to_a).unwrap();
}

#[test]
fn disjoint_creates_merge() {
    let mut a = Scene::new();
    let mut b = Scene::new();
    let id_a = a.create_object("rect", Author::Human).unwrap();
    let id_b = b.create_object("particles", Author::Agent).unwrap();
    assert_ne!(id_a, id_b, "client-id prefix keeps ids collision-free");

    sync_both(&a, &b);

    assert_eq!(a.len().expect("read replica A length"), 2);
    assert_eq!(b.len().expect("read replica B length"), 2);
    assert_eq!(
        a.object_ids().expect("read replica A ids"),
        b.object_ids().expect("read replica B ids")
    );
}

#[test]
fn configured_create_publishes_one_complete_update() {
    let updates = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let captured = updates.clone();
    let mut source = Scene::new();
    let _subscription = source
        .on_update(move |update| {
            captured
                .lock()
                .expect("captured update mutex poisoned")
                .push(update.to_vec());
        })
        .expect("install update observer");
    let blob = BlobRef {
        blob_id: 9,
        generation: 2,
        count: 6,
        dtype: DType::F32,
    };

    let id = source
        .create_object_with_props_and_blob(
            "particles",
            Author::Agent,
            &[
                ("x", PropValue::Num(36.0)),
                ("y", PropValue::Num(-54.0)),
                ("color", PropValue::Num(0x123456FF_u32 as f64)),
            ],
            blob,
        )
        .expect("create configured particle object");

    let updates = updates.lock().expect("captured update mutex poisoned");
    assert_eq!(updates.len(), 1, "one action must publish one CRDT update");
    let replica = Scene::new();
    replica
        .apply_update(&updates[0])
        .expect("apply the sole published update");
    let objects = replica
        .snapshot()
        .expect("read configured replica snapshot");
    assert_eq!(objects.len(), 1);
    assert_eq!(objects[0].id, id);
    assert_eq!(objects[0].kind, "particles");
    assert_eq!(objects[0].owner, "agent");
    assert_eq!((objects[0].x, objects[0].y), (36.0, -54.0));
    assert_eq!(objects[0].color, 0x123456FF);
    assert_eq!(objects[0].blob_ref, Some(blob));
}

#[test]
fn snapshot_orders_by_z_index_then_id_and_rejects_non_finite_data() {
    let mut scene = Scene::new();
    let low = scene
        .create_object("line", Author::Agent)
        .expect("create low object");
    let first_high = scene
        .create_object("disc", Author::Agent)
        .expect("create first high object");
    let second_high = scene
        .create_object("disc", Author::Agent)
        .expect("create second high object");
    let hostile = scene
        .create_object("label", Author::Agent)
        .expect("create hostile object");
    scene
        .set_prop(&low, "z_index", PropValue::Num(5.0))
        .expect("set low z-index");
    scene
        .set_prop(&first_high, "z_index", PropValue::Num(10.0))
        .expect("set high z-index");
    scene
        .set_prop(&second_high, "z_index", PropValue::Num(10.0))
        .expect("set high z-index");
    scene
        .set_prop(&hostile, "z_index", PropValue::Num(20.0))
        .expect("set hostile fixture z-index");
    assert!(scene
        .set_prop(&hostile, "z_index", PropValue::Num(f64::NAN))
        .is_err_and(|error| error.to_string().contains("must be finite")));

    let snapshot = scene.snapshot().expect("read ordered scene snapshot");
    assert_eq!(snapshot[0].id, low);
    let mut tied = [first_high, second_high];
    tied.sort();
    assert_eq!(snapshot[1].id, tied[0]);
    assert_eq!(snapshot[2].id, tied[1]);
    assert_eq!(snapshot[3].id, hostile);

    let mut txn = scene.doc().transact_mut();
    let objects = txn.get_map("objects").expect("objects root");
    let Out::YMap(object) = objects.get(&txn, hostile.as_str()).expect("hostile object") else {
        panic!("hostile fixture must be a map");
    };
    object.insert(&mut txn, "z_index", f64::NAN);
    drop(txn);
    assert!(scene
        .snapshot()
        .is_err_and(|error| error.to_string().contains("finite number")));
}

#[test]
fn malformed_crdt_blob_numbers_do_not_coerce_into_valid_references() {
    let mut scene = Scene::new();
    let id = scene
        .create_object("particles", Author::Human)
        .expect("create particle fixture");
    let valid = [
        ("blob_id", PropValue::Str("0000000000000007".into())),
        ("blob_gen", PropValue::Num(1.0)),
        ("blob_count", PropValue::Num(8.0)),
        ("blob_dtype", PropValue::Num(1.0)),
    ];
    scene.set_props(&id, &valid).expect("set valid blob ref");
    assert!(scene
        .get_blob_ref(&id)
        .expect("read valid blob reference")
        .is_some());

    for (key, bad) in [
        ("blob_gen", -1.0),
        ("blob_gen", 1.5),
        ("blob_count", f64::from(u32::MAX) + 1.0),
        ("blob_dtype", 257.0),
        ("blob_count", f64::NAN),
    ] {
        scene
            .set_props(&id, &valid)
            .expect("restore valid blob ref");
        let mut txn = scene.doc().transact_mut();
        let objects = txn.get_map("objects").expect("objects root");
        let Out::YMap(object) = objects.get(&txn, id.as_str()).expect("particle object") else {
            panic!("particle fixture must be a map");
        };
        object.insert(&mut txn, key, bad);
        drop(txn);
        assert!(
            scene.get_blob_ref(&id).is_err(),
            "{key}={bad:?} must fail closed"
        );
        assert!(
            scene.snapshot().is_err(),
            "{key}={bad:?} must fail snapshot"
        );
    }
}

#[test]
fn snapshot_rejects_non_map_root_entries() {
    let scene = Scene::new();
    let mut txn = scene.doc().transact_mut();
    let objects = txn.get_map("objects").expect("objects root");
    objects.insert(&mut txn, "hostile-scalar", 7.0);
    drop(txn);

    assert!(scene
        .snapshot()
        .is_err_and(|error| error.to_string().contains("is not a map")));
}

#[test]
fn local_writes_reject_invalid_state_before_mutation() {
    let mut scene = Scene::new();
    assert!(scene
        .create_object_with_props("disc", Author::Agent, &[("x", PropValue::Num(f64::NAN))],)
        .is_err());
    assert_eq!(scene.len().expect("scene length"), 0);

    let id = scene
        .create_object("disc", Author::Agent)
        .expect("first valid create");
    assert!(id.as_str().ends_with("-00000000"));
    assert!(scene
        .set_prop(&id, "x", PropValue::Str("not-a-number".into()))
        .is_err());
    assert!(scene
        .set_prop(&id, "blob_id", PropValue::Str("0000000000000007".into()),)
        .is_err());
    assert!(scene.snapshot().is_ok());

    let duplicate_blob_props = [
        ("blob_id", PropValue::Str("0000000000000007".into())),
        ("blob_gen", PropValue::Num(1.0)),
        ("blob_count", PropValue::Num(4.0)),
        ("blob_dtype", PropValue::Num(1.0)),
    ];
    assert!(scene
        .create_object_with_props_and_blob(
            "particles",
            Author::Agent,
            &duplicate_blob_props,
            BlobRef {
                blob_id: 7,
                generation: 1,
                count: 4,
                dtype: DType::F32,
            },
        )
        .is_err());
    assert_eq!(scene.len().expect("scene length"), 1);
}

#[test]
fn concurrent_different_keys_both_survive() {
    let mut a = Scene::new();
    let id = a.create_object("rect", Author::Human).unwrap();
    a.set_props(
        &id,
        &[("x", PropValue::Num(1.0)), ("color", PropValue::Num(1.0))],
    )
    .unwrap();

    let b = Scene::from_state(&a.encode_full().expect("encode base state")).unwrap();

    // Offline divergence: human moves it, agent recolors it.
    a.set_props(
        &id,
        &[("x", PropValue::Num(50.0)), ("y", PropValue::Num(60.0))],
    )
    .unwrap();
    b.set_prop(&id, "color", PropValue::Num(0xFF00FF00u32 as f64))
        .unwrap();

    sync_both(&a, &b);

    for scene in [&a, &b] {
        assert_eq!(
            scene
                .get_prop(&id, "x")
                .expect("read x")
                .and_then(|value| value.as_f64()),
            Some(50.0)
        );
        assert_eq!(
            scene
                .get_prop(&id, "y")
                .expect("read y")
                .and_then(|value| value.as_f64()),
            Some(60.0)
        );
        assert_eq!(
            scene
                .get_prop(&id, "color")
                .expect("read color")
                .and_then(|value| value.as_f64()),
            Some(0xFF00FF00u32 as f64),
            "agent's recolor must survive the human's concurrent move"
        );
    }
}

#[test]
fn concurrent_same_key_resolves_identically_everywhere() {
    let mut a = Scene::new();
    let id = a.create_object("rect", Author::Human).unwrap();
    let b = Scene::from_state(&a.encode_full().expect("encode base state")).unwrap();

    a.set_prop(&id, "x", PropValue::Num(111.0)).unwrap();
    b.set_prop(&id, "x", PropValue::Num(222.0)).unwrap();

    sync_both(&a, &b);

    let xa = a.get_prop(&id, "x").expect("read replica A x");
    let xb = b.get_prop(&id, "x").expect("read replica B x");
    // LWW: we don't assert WHICH write wins, only that every replica agrees.
    assert_eq!(xa, xb, "replicas must agree on the LWW winner");
    assert!(matches!(
        xa.and_then(|value| value.as_f64()),
        Some(111.0) | Some(222.0)
    ));
}

#[test]
fn delete_vs_concurrent_edit_converges() {
    let mut a = Scene::new();
    let id = a.create_object("rect", Author::Human).unwrap();
    let b = Scene::from_state(&a.encode_full().expect("encode base state")).unwrap();

    a.delete_object(&id).unwrap();
    b.set_prop(&id, "x", PropValue::Num(5.0)).unwrap();

    sync_both(&a, &b);

    assert_eq!(
        a.len().expect("read replica A length"),
        b.len().expect("read replica B length"),
        "replicas must agree on object count"
    );
    assert_eq!(
        a.object_ids().expect("read replica A ids"),
        b.object_ids().expect("read replica B ids")
    );
}

#[test]
fn clear_objects_deletes_imported_state_and_syncs_tombstones() {
    let mut client = Scene::new();
    client.create_object("rect", Author::Human).unwrap();
    client.create_object("label", Author::Agent).unwrap();

    // Model a fresh server learning pre-existing canvas state from a browser
    // reconnect. The server has no process-local creation registry for these.
    let server = Scene::from_state(&client.encode_full().expect("encode client state")).unwrap();
    assert_eq!(server.len().expect("read server length"), 2);

    let deleted = server.clear_objects().unwrap();
    assert_eq!(deleted.len(), 2);
    assert!(server.is_empty().expect("read cleared server state"));

    sync_both(&server, &client);
    assert!(
        client.is_empty().expect("read cleared client state"),
        "clear tombstones must reach the old replica"
    );
}

#[test]
fn differential_reconnect_is_smaller_than_full_state() {
    let mut a = Scene::new();
    for i in 0..50 {
        let id = a.create_object("rect", Author::Agent).unwrap();
        a.set_props(
            &id,
            &[
                ("x", PropValue::Num(i as f64)),
                ("y", PropValue::Num(i as f64 * 2.0)),
                ("color", PropValue::Num(0xFFFFFFFFu32 as f64)),
            ],
        )
        .unwrap();
    }
    // B was connected for the first 50 objects...
    let b = Scene::from_state(&a.encode_full().expect("encode base state")).unwrap();
    // ...then disconnected while A added one more.
    let id = a.create_object("rect", Author::Human).unwrap();
    a.set_prop(&id, "x", PropValue::Num(999.0)).unwrap();

    let full = a.encode_full().expect("encode full state");
    let diff = a
        .encode_diff(&b.state_vector().expect("read replica state vector"))
        .unwrap();
    assert!(
        diff.len() < full.len() / 4,
        "catch-up diff ({}) should be far smaller than full state ({})",
        diff.len(),
        full.len()
    );

    b.apply_update(&diff).unwrap();
    assert_eq!(
        a.len().expect("read source length"),
        b.len().expect("read replica length")
    );
    assert_eq!(
        b.get_prop(&id, "x")
            .expect("read replica x")
            .and_then(|value| value.as_f64()),
        Some(999.0)
    );
}

#[test]
fn blob_ref_roundtrips_through_sync() {
    let mut a = Scene::new();
    let id = a.create_object("particles", Author::Agent).unwrap();
    let blob = BlobRef {
        blob_id: 0xAB_CDEF_0123,
        generation: 4,
        count: 200_000,
        dtype: DType::F32,
    };
    a.set_blob_ref(&id, blob).unwrap();

    let b = Scene::from_state(&a.encode_full().expect("encode blob state")).unwrap();
    assert_eq!(
        b.get_blob_ref(&id).expect("read replica blob reference"),
        Some(blob)
    );

    let snap = b.snapshot().expect("read blob snapshot");
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].blob_ref, Some(blob));
    assert_eq!(snap[0].kind, "particles");
}

#[test]
fn ysync_handshake_via_handle_payload() {
    // Full y-sync handshake through the sync driver, as the server/client
    // will actually run it.
    let mut a = Scene::new();
    let id = a.create_object("rect", Author::Human).unwrap();
    a.set_prop(&id, "x", PropValue::Num(7.0)).unwrap();
    let b = Scene::new();

    // B connects: sends SyncStep1; A answers with SyncStep2; B applies it.
    let replies_from_a = handle_payload(&a, &greeting(&b).expect("encode greeting")).unwrap();
    assert_eq!(replies_from_a.len(), 1, "SyncStep1 yields one SyncStep2");
    for reply in &replies_from_a {
        assert!(handle_payload(&b, reply).unwrap().is_empty());
    }
    assert_eq!(
        b.get_prop(&id, "x")
            .expect("read synced x")
            .and_then(|value| value.as_f64()),
        Some(7.0)
    );

    // Steady state: a live local edit on A reaches B as an Update message.
    a.set_prop(&id, "x", PropValue::Num(8.0)).unwrap();
    let update = a
        .encode_diff(&b.state_vector().expect("read replica state vector"))
        .unwrap();
    let msg = update_message(&update);
    assert!(handle_payload(&b, &msg).unwrap().is_empty());
    assert_eq!(
        b.get_prop(&id, "x")
            .expect("read updated x")
            .and_then(|value| value.as_f64()),
        Some(8.0)
    );
}

#[test]
fn on_update_fires_with_appliable_bytes() {
    use std::sync::{Arc, Mutex};

    let mut a = Scene::new();
    let captured: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    let sink = captured.clone();
    let _sub = a
        .on_update(move |bytes| sink.lock().unwrap().push(bytes.to_vec()))
        .unwrap();

    let id = a.create_object("rect", Author::Human).unwrap();
    a.set_prop(&id, "x", PropValue::Num(3.5)).unwrap();

    let updates = captured.lock().unwrap().clone();
    assert!(
        updates.len() >= 2,
        "create + set_prop should each commit an update"
    );

    // The captured bytes must reconstruct the same state on a fresh replica.
    let b = Scene::new();
    for u in &updates {
        b.apply_update(u).unwrap();
    }
    assert_eq!(
        b.get_prop(&id, "x")
            .expect("read reconstructed x")
            .and_then(|value| value.as_f64()),
        Some(3.5)
    );
}

#[test]
fn authoritative_reads_fail_during_transaction_contention() {
    let mut scene = Scene::new();
    let id = scene
        .create_object("particles", Author::Agent)
        .expect("create contention fixture");
    scene
        .set_blob_ref(
            &id,
            BlobRef {
                blob_id: 7,
                generation: 1,
                count: 8,
                dtype: DType::F32,
            },
        )
        .expect("configure contention fixture");
    let remote_state_vector = scene
        .state_vector()
        .expect("read state vector before contention");
    let remote_state_vector_v1 = scene
        .state_vector_v1()
        .expect("encode state vector before contention");

    let _held_write = scene.doc().transact_mut();

    assert!(scene.get_prop(&id, "kind").is_err());
    assert!(scene.get_blob_ref(&id).is_err());
    assert!(scene.object_ids().is_err());
    assert!(scene.len().is_err());
    assert!(scene.is_empty().is_err());
    assert!(scene.snapshot().is_err());
    assert!(scene.state_vector().is_err());
    assert!(scene.state_vector_v1().is_err());
    assert!(scene.encode_diff(&remote_state_vector).is_err());
    assert!(scene.encode_diff_v1(&remote_state_vector_v1).is_err());
    assert!(scene.encode_full().is_err());
    assert!(scene.compact_snapshot().is_err());
    assert!(greeting(&scene).is_err());
}
