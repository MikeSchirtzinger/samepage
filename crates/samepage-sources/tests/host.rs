use samepage_capability::{
    Batch, BatchEntry, Call, Capability, Change, Composition, Op, Plan, Record, Target, View,
};
use samepage_doc::{
    Actor, Caller,
    host::{Host, Verifier},
};
use samepage_map::Map;
use samepage_review::Review;
use samepage_sources::{Binding, Facts, Sources};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
#[allow(dead_code)]
#[path = "../../../apps/samepage/src/host/sources.rs"]
mod native;
static SEQ: AtomicU64 = AtomicU64::new(0);
fn root() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "sources-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("a.rs"),
        "pub enum State { Ready, Done }\npub fn run() {}\n",
    )
    .unwrap();
    root
}
fn agent() -> Actor {
    Actor::new("sources-agent", Caller::Agent).labelled("Sources worker")
}
fn host(root: &std::path::Path) -> Host {
    Host::in_memory(
        Composition::new(vec![
            samepage_map::descriptor(),
            samepage_sources::descriptor(),
            samepage_review::descriptor(),
        ])
        .unwrap(),
        vec![Arc::new(Map), Arc::new(Sources), Arc::new(Review)],
    )
    .unwrap()
    .with_verifier("sources", Arc::new(native::Repository::open(root).unwrap()))
    .unwrap()
    .with_verifier(
        "review",
        Arc::new(Claims(native::Repository::open(root).unwrap())),
    )
    .unwrap()
}
struct Claims(native::Repository);
impl Verifier for Claims {
    fn verify(&self, c: &Change<'_>) -> Result<Value, samepage_capability::CapError> {
        let mut f = serde_json::Map::new();
        for id in c.attempted.keys() {
            if let Some(r) = c
                .after
                .get(id)
                .filter(|r| r.kind == "claim" && samepage_sources::string(r, "basis") == "verified")
            {
                let p = samepage_sources::string(r, "path");
                let l = samepage_sources::string(r, "lines");
                let rev = samepage_sources::string(r, "revision");
                self.0.source(p, l, rev)?;
                f.insert(id.clone(), json!({"path":p,"lines":l,"revision":rev}));
            }
        }
        Ok(Value::Object(f))
    }
}
fn place(
    h: &Host,
    args: Value,
) -> Result<samepage_doc::host::Invoked, samepage_doc::host::InvokeError> {
    let b = samepage_sources::place_batch(
        &Call {
            action: "card_place".into(),
            args,
        },
        &h.view(),
    )
    .unwrap();
    h.invoke_composed(&agent(), "atlas_place", &b)
}
fn fingerprint(h: &Host) -> (String, Vec<u8>, u64) {
    (h.revision(), h.encode_full(), h.journal().seq)
}
#[test]
fn diagrams_transfer_source_ownership_and_publish_only_after_verification() {
    let r = root();
    let diagrams = samepage_diagrams::WithSources;
    let h = Host::in_memory(
        Composition::new(vec![
            samepage_map::descriptor(),
            samepage_sources::descriptor(),
            diagrams.descriptor(),
        ])
        .unwrap(),
        vec![Arc::new(Map), Arc::new(Sources), Arc::new(diagrams)],
    )
    .unwrap()
    .with_verifier("sources", Arc::new(native::Repository::open(&r).unwrap()))
    .unwrap();
    let made = h
        .invoke(
            &agent(),
            "diagram_draw",
            json!({"nodes":[{"id":"state","label":"State","path":"a.rs","lines":"1-2"}]}),
        )
        .unwrap();
    assert_eq!(made.commit.unwrap().created.len(), 1);
    assert_eq!(h.journal().seq, 1);
    let before = fingerprint(&h);
    assert!(
        h.invoke(
            &agent(),
            "diagram_draw",
            json!({"nodes":[{"id":"bad","label":"Absent","path":"missing.rs","lines":"1"}]})
        )
        .is_err()
    );
    assert_eq!(fingerprint(&h), before);
    std::fs::remove_dir_all(r).unwrap();
}
#[test]
fn viewer_browses_bounded_pages_and_preserves_exact_linked_excerpts() {
    let r = root();
    std::fs::write(
        r.join("large.rs"),
        (1..=600)
            .map(|n| format!("// line {n}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let repo = native::Repository::open(&r).unwrap();
    let first = repo.excerpt("large.rs", "", "").unwrap();
    assert_eq!(
        (
            first.first_line,
            first.total_lines,
            first.lines.as_str(),
            first.truncated
        ),
        (1, 600, "1-400", true)
    );
    let second = repo.excerpt("large.rs", "401-600", "").unwrap();
    assert_eq!(second.text.lines().count(), 200);
    let linked = repo.excerpt("a.rs", "2", "").unwrap();
    assert_eq!(linked.text, "pub fn run() {}\n");
    assert_eq!(
        samepage_sources::viewer(&linked).unwrap()["lines"][0]["number"],
        2
    );
    assert!(repo.excerpt("large.rs", "601", "").is_err());
    let revision = commit(&r);
    std::fs::remove_file(r.join("large.rs")).unwrap();
    let archive = repo.excerpt("large.rs", "", &revision).unwrap();
    assert_eq!(
        (archive.total_lines, archive.lines.as_str()),
        (600, "1-400")
    );
    assert_eq!(
        repo.excerpt("large.rs", "401-600", &revision)
            .unwrap()
            .text
            .lines()
            .count(),
        200
    );
    std::fs::remove_dir_all(r).unwrap();
}
#[test]
fn package_entry_points_are_real_readable_files() {
    let r = root();
    std::fs::write(
        r.join("Cargo.toml"),
        "[package]\nname = 'example'\nversion = '0.1.0'\n[lib]\npath = 'a.rs'\n",
    )
    .unwrap();
    let repo = native::Repository::open(&r).unwrap();
    assert_eq!(repo.entry_points("Cargo.toml").unwrap(), vec!["a.rs"]);
    std::fs::write(r.join("Cargo.toml"), "[lib]\npath = '../escape.rs'\n").unwrap();
    assert!(repo.entry_points("Cargo.toml").unwrap().is_empty());
    std::fs::remove_dir_all(r).unwrap();
}
#[test]
fn refuses_to_leave_the_root() {
    let r = root();
    let repo = native::Repository::open(&r).unwrap();
    for bad in [
        "../../etc/passwd",
        "/etc/passwd",
        "~/.ssh/id_rsa",
        ".git/config",
        "target/debug",
    ] {
        assert!(repo.read(bad, "1").is_err(), "{bad}");
        assert!(repo.source(bad, "1", &"a".repeat(40)).is_err());
    }
    std::os::unix::fs::symlink("/etc/passwd", r.join("escape.rs")).unwrap();
    assert!(repo.read("escape.rs", "1").is_err());
    std::fs::remove_dir_all(r).unwrap();
}
#[test]
fn refuses_credential_shaped_and_binary_files() {
    let r = root();
    for name in ["auth.json", ".env", "secret.rs", "image.png"] {
        std::fs::write(r.join(name), "valid text\n").unwrap();
    }
    std::fs::write(r.join("binary.rs"), b"abc\0def\n").unwrap();
    std::fs::write(r.join("invalid.rs"), [255]).unwrap();
    std::os::unix::fs::symlink(r.join("auth.json"), r.join("alias.rs")).unwrap();
    let repo = native::Repository::open(&r).unwrap();
    for name in [
        "auth.json",
        ".env",
        "secret.rs",
        "image.png",
        "binary.rs",
        "invalid.rs",
        "alias.rs",
    ] {
        assert!(repo.read(name, "1").is_err(), "{name}");
    }
    let names = repo.list("").unwrap().to_string();
    assert!(!names.contains("auth.json"));
    assert!(!names.contains("alias.rs"));
    std::fs::remove_dir_all(r).unwrap();
}
#[test]
fn a_stale_line_range_is_an_error_not_an_empty_excerpt() {
    let r = root();
    let repo = native::Repository::open(&r).unwrap();
    for range in ["9000-9100", "1-3"] {
        let error = repo.read("a.rs", range).unwrap_err().to_string();
        assert!(
            error.contains("lines, so this range ends past it"),
            "{error}"
        );
    }
    let lines = std::fs::read_to_string(r.join("a.rs"))
        .unwrap()
        .lines()
        .count();
    assert_eq!(repo.line_count("a.rs").unwrap(), lines);
    assert!(
        repo.read("a.rs", "1-3")
            .unwrap_err()
            .to_string()
            .contains(&format!("has {lines} lines"))
    );
    std::fs::remove_dir_all(r).unwrap();
}
#[test]
fn effective_bindings_is_the_primary_binding_plus_claimed_ones_deduplicated() {
    let n = Record::new("n1", "node")
        .with("path", "src/main.rs")
        .with("lines", "4");
    let p = Binding {
        path: "src/main.rs".into(),
        lines: "4".into(),
    };
    let extra = Binding {
        path: p.path.clone(),
        lines: "12".into(),
    };
    assert_eq!(
        samepage_sources::effective_bindings(&n, &[extra.clone(), p.clone(), extra.clone()]),
        vec![p, extra]
    );
}
#[test]
fn failed_sources_leave_no_record_author_stamp_journal_or_broadcast() {
    let r = root();
    let h = host(&r);
    let broadcasts = Arc::new(AtomicU64::new(0));
    let observed = broadcasts.clone();
    h.on_commit(move |_| {
        observed.fetch_add(1, Ordering::Relaxed);
    });
    let before = fingerprint(&h);
    assert!(place(&h, json!({"label":"Bad","path":"missing.rs","lines":"1"})).is_err());
    assert_eq!(fingerprint(&h), before);
    assert_eq!(broadcasts.load(Ordering::Relaxed), 0);
    let made = place(&h, json!({"label":"State","path":"a.rs","lines":"1"})).unwrap();
    assert_eq!(made.commit.unwrap().created.len(), 1);
    assert_eq!(h.journal().seq, 1);
    assert_eq!(
        h.journal().recent.back().unwrap().action.as_deref(),
        Some("card_place")
    );
    let id = &made.ids["card"];
    assert_eq!(
        h.view().get(id).unwrap().props["created_by_participant"].as_str(),
        Some("sources-agent")
    );
    let before = fingerprint(&h);
    assert!(place(&h, json!({"id":id,"lines":"3"})).is_err());
    assert_eq!(fingerprint(&h), before);
    assert_eq!(broadcasts.load(Ordering::Relaxed), 1);
    std::fs::remove_dir_all(r).unwrap();
}
#[test]
fn replica_and_untrusted_batch_lines_only_updates_are_verified() {
    let r = root();
    let h = host(&r);
    let id = place(&h, json!({"label":"State","path":"a.rs","lines":"1"}))
        .unwrap()
        .ids["card"]
        .clone();
    let mut peer = samepage_doc::Replica::new();
    peer.set_composition(vec![
        samepage_map::descriptor(),
        samepage_sources::descriptor(),
        samepage_review::descriptor(),
    ])
    .unwrap();
    for f in h.hello() {
        peer.receive(&f).unwrap();
    }
    let b = Batch {
        base_revision: h.revision(),
        entries: vec![BatchEntry {
            capability: "sources".into(),
            plan: Plan {
                ops: vec![Op::Set {
                    target: Target::Id(id),
                    props: [("lines".into(), "999".into())].into(),
                }],
                readback: String::new(),
            },
        }],
    };
    let before = fingerprint(&h);
    assert!(h.commit_batch(&agent(), &b).is_err());
    assert_eq!(fingerprint(&h), before);
    peer.apply_batch(&b).unwrap();
    let who = Actor::new("browser", Caller::Human);
    let results: Vec<_> = peer
        .take_outbound()
        .into_iter()
        .map(|f| h.receive_frame(&who, &f))
        .collect();
    assert!(results.iter().any(Result::is_err));
    assert_eq!(fingerprint(&h), before);
    std::fs::remove_dir_all(r).unwrap();
}
#[test]
fn source_facts_pin_drift_and_survive_restart() {
    let r = root();
    let h = host(&r);
    let id = place(&h, json!({"label":"State","path":"a.rs","lines":"1"}))
        .unwrap()
        .ids["card"]
        .clone();
    let state = r.join("document.json");
    let s = native::SourcesHost::open(&r, &state).unwrap();
    assert_eq!(
        s.inspect(&h.view()).unwrap()["projection"]["cards"][&id]["tier"],
        "verified"
    );
    std::fs::write(
        r.join("a.rs"),
        "pub enum State { Ready, Lost }\npub fn run() {}\n",
    )
    .unwrap();
    let s = native::SourcesHost::open(&r, &state).unwrap();
    assert_eq!(
        s.inspect(&h.view()).unwrap()["projection"]["cards"][&id]["tier"],
        "drifted"
    );
    std::fs::remove_file(r.join("a.rs")).unwrap();
    assert_eq!(
        s.inspect(&h.view()).unwrap()["projection"]["cards"][&id]["tier"],
        "claimed"
    );
    std::fs::remove_dir_all(r).unwrap();
}
#[test]
fn tier_precedence_is_proposed_gate_unreadable_drifted_verified() {
    let n = Record::new("n", "node")
        .with("label", "State")
        .with("path", "a.rs")
        .with("lines", "1");
    let v = View::new("r", [n.clone()]).unwrap();
    let key = samepage_sources::binding_key(
        "n",
        &Binding {
            path: "a.rs".into(),
            lines: "1".into(),
        },
    );
    let mut f = Facts::default();
    f.bindings.insert(
        key.clone(),
        samepage_sources::BindingFact {
            readable: true,
            current_sha256: Some("abc123".into()),
            baseline_sha256: Some("abc123".into()),
            error: None,
        },
    );
    assert_eq!(
        samepage_sources::project(&v, &f).unwrap()["cards"]["n"]["tier"],
        "verified"
    );
    f.bindings.get_mut(&key).unwrap().current_sha256 = Some("changed".into());
    assert_eq!(
        samepage_sources::project(&v, &f).unwrap()["cards"]["n"]["tier"],
        "drifted"
    );
    f.bindings.get_mut(&key).unwrap().readable = false;
    assert_eq!(
        samepage_sources::project(&v, &f).unwrap()["cards"]["n"]["tier"],
        "claimed"
    );
    f.gates.insert(
        "n".into(),
        samepage_sources::GateFact {
            obligation_id: "GATE".into(),
            met: true,
            reason: "checked".into(),
            blocker: String::new(),
        },
    );
    assert_eq!(
        samepage_sources::project(&v, &f).unwrap()["cards"]["n"]["tier"],
        "gate-met"
    );
    f.gates.insert(
        "n".into(),
        samepage_sources::GateFact {
            obligation_id: "GATE".into(),
            met: false,
            reason: "UNATTESTED: GATE passes its check but has no fresh attestation".into(),
            blocker: "unattested".into(),
        },
    );
    let projection = samepage_sources::project(&v, &f).unwrap();
    assert_eq!(projection["cards"]["n"]["tier"], "gate-unmet");
    assert_eq!(projection["cards"]["n"]["chip"], "GATE UNMET: UNATTESTED");
    assert!(
        projection["readback"]
            .as_str()
            .unwrap()
            .contains("GATE UNMET: UNATTESTED; source a.rs:1 (UNATTESTED: GATE passes"),
        "{}",
        projection["readback"]
    );
    let v = View::new(
        "r",
        [
            n,
            Record::new("e", "edge")
                .with("from", "n")
                .with("to", "other")
                .with("label", "replaces"),
        ],
    )
    .unwrap();
    assert_eq!(
        samepage_sources::project(&v, &f).unwrap()["cards"]["n"]["tier"],
        "proposed"
    );
}
fn commit(r: &std::path::Path) -> String {
    for args in [
        vec!["init", "--quiet"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Sources proof",
            "-c",
            "user.email=sources@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "Source baseline",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(r)
                .status()
                .unwrap()
                .success()
        );
    }
    String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(r)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .into()
}
fn claim(
    h: &Host,
    args: Value,
) -> Result<samepage_doc::host::Invoked, samepage_doc::host::InvokeError> {
    h.invoke(&agent(), "claim_add", args)
}
#[test]
fn atlas_claim_refuses_a_verified_claim_whose_range_does_not_hold() {
    let r = root();
    let h = host(&r);
    let id = place(&h, json!({"label":"State"})).unwrap().ids["card"].clone();
    let before = fingerprint(&h);
    assert!(
        claim(
            &h,
            json!({"about":id,"text":"stale","basis":"verified","path":"a.rs","lines":"999999"})
        )
        .is_err()
    );
    assert_eq!(fingerprint(&h), before);
    std::fs::remove_dir_all(r).unwrap();
}
#[test]
fn atlas_claim_and_its_revise_path_verify_a_present_revision_with_git() {
    let r = root();
    let rev = commit(&r);
    let h = host(&r);
    let about = place(&h, json!({"label":"State"})).unwrap().ids["card"].clone();
    std::fs::remove_file(r.join("a.rs")).unwrap();
    let id=claim(&h,json!({"about":about,"text":"State exists","basis":"verified","path":"a.rs","lines":"1","revision":rev})).unwrap().ids["claim"].clone();
    claim(&h,json!({"id":id,"about":about,"text":"State and run exist","basis":"verified","path":"a.rs","lines":"1-2","revision":rev})).unwrap();
    assert_eq!(
        h.view().get(&id).unwrap().props["lines"].as_str(),
        Some("1-2")
    );
    std::fs::remove_dir_all(r).unwrap();
}
fn refused_revision(path: &str, lines: &str, missing: bool) -> String {
    let r = root();
    let rev = commit(&r);
    let h = host(&r);
    let about = place(&h, json!({"label":"State"})).unwrap().ids["card"].clone();
    let before = fingerprint(&h);
    let e=claim(&h,json!({"about":about,"text":"missing","basis":"verified","path":path,"lines":lines,"revision":if missing {"f".repeat(40)}else{rev}})).unwrap_err().to_string();
    assert_eq!(fingerprint(&h), before);
    std::fs::remove_dir_all(r).unwrap();
    e
}
#[test]
fn atlas_claim_refuses_a_revision_missing_from_git() {
    assert!(refused_revision("a.rs", "1", true).contains("is not a commit in this repository"));
}
#[test]
fn atlas_claim_refuses_a_path_missing_at_a_revision() {
    assert!(
        refused_revision("missing.rs", "1", false).contains("does not exist as a file at revision")
    );
}
#[test]
fn atlas_claim_refuses_a_range_past_the_end_at_a_revision() {
    assert!(refused_revision("a.rs", "1-399", false).contains("ends past the end"));
}
