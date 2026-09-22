//! Persistence and migration: saved documents reopen exactly; records and
//! state the running app does not understand are preserved untouched.

mod support;

use base64::Engine as _;
use samepage_capability::Scalar;
use samepage_doc::host::{Host, OpenError};
use samepage_doc::Document;
use serde_json::json;
use support::*;
use yrs::updates::decoder::Decode;
use yrs::{Any, Array, Doc, Map, MapPrelim, ReadTxn, Transact};

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("samepage-doc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_state(path: &std::path::Path, update: &[u8]) {
    std::fs::write(
        path,
        base64::engine::general_purpose::STANDARD.encode(update),
    )
    .unwrap();
}

/// A document the way the Atlas scene layer wrote it: legacy `owner` keys,
/// kinds this app has no capability for, an extra root, and a nested value.
fn legacy_document() -> Vec<u8> {
    let doc = Doc::new();
    let objects = doc.get_or_insert_map("objects");
    let extra = doc.get_or_insert_array("legacy-log");
    let mut txn = doc.transact_mut();
    let card = objects.insert(&mut txn, "n1", MapPrelim::default());
    card.insert(&mut txn, "kind", "card");
    card.insert(&mut txn, "owner", "agent");
    card.insert(&mut txn, "created_by", "agent");
    card.insert(&mut txn, "label", "Legacy");
    card.insert(&mut txn, "state_variable", "cursor");
    let ink = objects.insert(&mut txn, "v1", MapPrelim::default());
    ink.insert(&mut txn, "kind", "agent-ink-variable");
    ink.insert(&mut txn, "value", 3.0);
    let nested = objects.insert(&mut txn, "s1", MapPrelim::default());
    nested.insert(&mut txn, "kind", "shape");
    nested.insert(
        &mut txn,
        "segment_parts",
        MapPrelim::from([("a".to_string(), Any::from(1.0))]),
    );
    extra.push_back(&mut txn, "entry");
    drop(txn);
    let bytes = doc
        .transact()
        .encode_state_as_update_v1(&yrs::StateVector::default());
    bytes
}

#[test]
fn save_and_reopen_keep_state_journal_and_registration() {
    let dir = temp_dir("roundtrip");
    let path = dir.join("page.json");
    let host = Host::open(&path, composition(), capabilities()).unwrap();
    let id = place(&host, "Kept");
    let mut peer = Peer::of(&host);
    {
        let objects = peer.objects();
        let mut txn = peer.doc.transact_mut();
        let Some(yrs::Out::YMap(card)) = objects.get(&txn, id.as_str()) else {
            panic!()
        };
        card.insert(&mut txn, "x", 44.0);
    }
    host.receive_frame(&alice(), &peer.frame()).unwrap();
    host.save().unwrap();
    let view = host.view();
    let revision = host.revision();
    let journal = host.journal();
    let first_host_client = host.client_id();
    drop(host);

    let reopened = Host::open(&path, composition(), capabilities()).unwrap();
    assert_eq!(reopened.view(), view);
    assert_eq!(reopened.revision(), revision);
    let again = reopened.journal();
    assert_eq!(again.seq, journal.seq);
    assert_eq!(again.bindings, journal.bindings);
    assert_ne!(
        reopened.client_id(),
        first_host_client,
        "a new process gets a new host client"
    );
    assert!(
        again.reserved.contains(&first_host_client),
        "old host ids stay reserved"
    );
    // Alice's registration survives: her client may keep writing.
    {
        let objects = peer.objects();
        let mut txn = peer.doc.transact_mut();
        let Some(yrs::Out::YMap(card)) = objects.get(&txn, id.as_str()) else {
            panic!()
        };
        card.insert(&mut txn, "x", 45.0);
    }
    reopened.receive_frame(&alice(), &peer.frame()).unwrap();
    assert_eq!(
        reopened.view().get(&id).unwrap().props["x"],
        Scalar::Num(45.0)
    );
}

#[test]
fn a_legacy_document_opens_and_everything_it_does_not_understand_is_preserved() {
    let dir = temp_dir("legacy");
    let legacy = dir.join("atlas.json");
    let legacy_bytes = legacy_document();
    write_state(&legacy, &legacy_bytes);
    let original = std::fs::read(&legacy).unwrap();
    // A legacy file is not opened as an envelope...
    assert!(matches!(
        Host::open(&legacy, composition(), capabilities()),
        Err(OpenError::Format { .. })
    ));
    // ...it is imported, read-only, into a new file.
    let path = dir.join("page.samepage");
    assert!(Host::import_legacy(&legacy, &legacy, composition(), capabilities()).is_err());
    let host = Host::import_legacy(&legacy, &path, composition(), capabilities()).unwrap();
    let anomalies = host.opened_anomalies().to_vec();
    assert!(
        anomalies.iter().any(|a| a == "root:legacy-log"),
        "{anomalies:?}"
    );
    assert!(
        anomalies.iter().any(|a| a == "value:s1:segment_parts"),
        "{anomalies:?}"
    );

    // Every client id already in the file is reserved.
    let loaded = Document::from_state(&legacy_bytes).unwrap();
    for (client, _) in loaded.state_vector().iter() {
        assert!(host.journal().reserved.contains(&client.get()));
    }

    // A legitimate edit to the known card keeps its unknown keys.
    host.invoke(&agent(), "card_move", json!({ "id": "n1", "x": 9.0 }))
        .unwrap();
    let n1 = host.view().get("n1").unwrap().clone();
    assert_eq!(n1.props["owner"], Scalar::from("agent"));
    assert_eq!(n1.props["state_variable"], Scalar::from("cursor"));
    assert_eq!(n1.props["x"], Scalar::Num(9.0));
    assert!(host.view().get("v1").is_some(), "unknown kind preserved");

    host.save().unwrap();
    let reopened = Host::open(&path, composition(), capabilities()).unwrap();
    assert_eq!(reopened.view(), host.view());
    assert_eq!(
        reopened.opened_anomalies(),
        &anomalies[..],
        "state outside the layout is byte-for-byte kept"
    );
    assert_eq!(
        std::fs::read(&legacy).unwrap(),
        original,
        "the legacy source is never written"
    );
    assert!(
        Host::import_legacy(&legacy, &path, composition(), capabilities()).is_err(),
        "import never overwrites"
    );
}

#[test]
fn a_document_with_unresolved_content_is_refused_rather_than_opened() {
    let source = Doc::new();
    let objects = source.get_or_insert_map("objects");
    let mark = {
        let mut txn = source.transact_mut();
        objects.insert(&mut txn, "a", MapPrelim::default());
        drop(txn);
        source.transact().state_vector()
    };
    {
        let mut txn = source.transact_mut();
        let Some(yrs::Out::YMap(a)) = objects.get(&txn, "a") else {
            panic!()
        };
        a.insert(&mut txn, "kind", "card");
    }
    // Only the second half: it depends on the first.
    let orphan = source.transact().encode_state_as_update_v1(&mark);
    let doc = Doc::new();
    doc.transact_mut()
        .apply_update(yrs::Update::decode_v1(&orphan).unwrap())
        .unwrap();
    // yrs keeps it pending and would export it.
    assert!(yrs::ReadTxn::has_missing_updates(&doc.transact()));
    let pending_export = doc
        .transact()
        .encode_state_as_update_v1(&yrs::StateVector::default());

    let dir = temp_dir("pending");
    let legacy = dir.join("pending.json");
    write_state(&legacy, &pending_export);
    assert!(matches!(
        Host::import_legacy(&legacy, dir.join("out"), composition(), capabilities()),
        Err(OpenError::Unresolved { .. })
    ));
}

#[test]
fn a_state_file_that_is_not_base64_is_a_format_error() {
    let dir = temp_dir("format");
    let path = dir.join("bad.json");
    std::fs::write(&path, "{\"not\": \"a document\"}").unwrap();
    assert!(matches!(
        Host::open(&path, composition(), capabilities()),
        Err(OpenError::Format { .. })
    ));
}

/// Real saved documents, opened read-only from private copies. Run with
/// `SAMEPAGE_DOC_MIGRATION_FILES=<path>:<path> cargo test -- --ignored`.
/// Each file is copied into a temporary directory first; the original is only
/// read.
#[test]
#[ignore = "needs private document copies; run explicitly"]
fn real_saved_documents_open_roundtrip_and_accept_an_edit() {
    let files = std::env::var("SAMEPAGE_DOC_MIGRATION_FILES")
        .expect("set SAMEPAGE_DOC_MIGRATION_FILES to run this test");
    for (index, original) in files.split(':').filter(|p| !p.is_empty()).enumerate() {
        let original = std::path::Path::new(original);
        let bytes = std::fs::read(original).unwrap();
        let dir = temp_dir(&format!("migration-{index}"));
        let source = dir.join("legacy.state");
        std::fs::write(&source, &bytes).unwrap();
        let copy = dir.join("imported.samepage");

        let host = Host::import_legacy(&source, &copy, composition(), capabilities())
            .unwrap_or_else(|error| panic!("{}: {error}", original.display()));
        let view = host.view();
        let revision = host.revision();
        let anomalies = host.opened_anomalies().to_vec();
        let mut kinds: Vec<String> = view.records().map(|r| r.kind.clone()).collect();
        kinds.sort();
        kinds.dedup();
        println!(
            "MIGRATION {}: {} bytes, {} records, kinds {:?}, {} anomalies {:?}, revision {}",
            original.display(),
            bytes.len(),
            view.len(),
            kinds,
            anomalies.len(),
            anomalies,
            revision
        );

        // Roundtrip without edits: identical records and revision.
        host.save().unwrap();
        let reopened = Host::open(&copy, composition(), capabilities()).unwrap();
        assert_eq!(reopened.view(), view);
        assert_eq!(reopened.revision(), revision);

        // An edit through the kernel keeps every other record untouched.
        let id = place(&reopened, "migration check");
        let after = reopened.view();
        for record in view.records() {
            assert_eq!(after.get(&record.id), Some(record), "{} changed", record.id);
        }
        assert!(after.get(&id).is_some());
        reopened.save().unwrap();
        let third = Host::open(&copy, composition(), capabilities()).unwrap();
        assert_eq!(third.view(), after);
        assert_eq!(third.opened_anomalies(), &anomalies[..]);
        assert_eq!(
            std::fs::read(original).unwrap(),
            bytes,
            "the original was not modified"
        );
        assert_eq!(
            std::fs::read(&source).unwrap(),
            bytes,
            "the imported copy was not modified"
        );
    }
}

#[test]
fn saves_are_one_envelope_serialized_in_generation_order() {
    let dir = temp_dir("concurrent");
    let path = dir.join("page.samepage");
    let host = std::sync::Arc::new(Host::open(&path, composition(), capabilities()).unwrap());
    let threads: Vec<_> = (0..8)
        .map(|n| {
            let host = host.clone();
            std::thread::spawn(move || {
                place(&host, &format!("card {n}"));
                host.save().unwrap()
            })
        })
        .collect();
    let mut generations: Vec<u64> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    generations.sort();
    assert_eq!(generations, (1..=8).collect::<Vec<_>>());
    let final_generation = host.save().unwrap();
    let reopened = Host::open(&path, composition(), capabilities()).unwrap();
    assert_eq!(reopened.generation(), final_generation);
    assert_eq!(reopened.view(), host.view());
    assert_eq!(reopened.journal().seq, host.journal().seq);
    // No temporary files are left behind.
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn a_failed_save_or_a_crashed_temporary_never_changes_what_opens() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("failed-save");
    let path = dir.join("page.samepage");
    let host = Host::open(&path, composition(), capabilities()).unwrap();
    place(&host, "saved");
    host.save().unwrap();
    let saved = std::fs::read(&path).unwrap();
    let saved_view = host.view();

    // A crash mid-write leaves a partial temporary; opening ignores it.
    std::fs::write(
        dir.join(".page.samepage.999.0.tmp"),
        &saved[..saved.len() / 2],
    )
    .unwrap();
    assert_eq!(
        Host::open(&path, composition(), capabilities())
            .unwrap()
            .view(),
        saved_view
    );

    // A save that cannot write fails and leaves the previous envelope whole.
    place(&host, "unsaved");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = host.save();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    let reopened = Host::open(&path, composition(), capabilities()).unwrap();
    assert_eq!(reopened.view(), saved_view);
    // State and journal always come from the same envelope.
    assert_eq!(reopened.journal().seq as usize, saved_view.len());
}
