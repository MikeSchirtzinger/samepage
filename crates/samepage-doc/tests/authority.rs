//! The host authority hook: who changed what, by which route, from which
//! state, with which native facts. Fixture capabilities state their policies
//! in `validate_change`; the kernel supplies the context on every route.

mod support;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use samepage_capability::{
    ActionDecl, Artifact, Audience, Batch, BatchEntry, Call, CallerKind, CapError, Capability,
    Change, Composition, Delta, Descriptor, Effect, Op, Origin, Plan, Principal, Scalar, Target,
    Value, View,
};
use samepage_doc::host::{CommitError, FrameOutcome, FrameRefusal, Host, InvokeError, Verifier};
use samepage_doc::{Actor, Caller};
use serde_json::json;
use support::*;
use yrs::{Map as _, MapPrelim, Transact};

// ── fixture: claims with human verdicts ──────────────────────────────────

/// What the claims capability saw in each `validate_change`.
#[derive(Debug, Clone)]
struct Seen {
    principal: Principal,
    origin: Origin,
    attempted: Vec<(String, Vec<String>)>,
    delta: Delta,
    before_ids: BTreeSet<String>,
    after_ids: BTreeSet<String>,
}

#[derive(Clone, Default)]
struct Claims {
    seen: Arc<Mutex<Vec<Seen>>>,
}

fn str_of<'a>(view: &'a View, id: &str, key: &str) -> Option<&'a str> {
    view.get(id)
        .and_then(|r| r.props.get(key))
        .and_then(|v| v.as_str())
}

const SETTLED: &[&str] = &["accepted", "rejected"];

impl Claims {
    fn descriptor() -> Descriptor {
        Descriptor::new("claims", "0.1.0", Artifact::Wasm)
            .depends_on("cards", "0.1.0", "claims are about cards")
            .owns(
                "claim",
                &["card", "text", "verdict", "withdrawn"],
                "claims and verdicts",
            )
            .reads("card")
            .action(
                ActionDecl::new("claim_open", Audience::Both, Effect::Mutate, "Open.")
                    .alias("atlas_claim"),
            )
            .action(ActionDecl::new(
                "claim_verdict",
                Audience::Human,
                Effect::Mutate,
                "Verdict.",
            ))
            .action(ActionDecl::new(
                "claim_rewrite",
                Audience::Both,
                Effect::Mutate,
                "Rewrite.",
            ))
            .action(ActionDecl::new(
                "claim_withdraw",
                Audience::Both,
                Effect::Mutate,
                "Withdraw.",
            ))
    }
}

impl Capability for Claims {
    fn descriptor(&self) -> Descriptor {
        Self::descriptor()
    }
    fn project(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
    fn plan(&self, call: &Call, view: &View) -> Result<Plan, CapError> {
        let text = |key: &str| call.args[key].as_str().unwrap_or_default().to_string();
        let set = |id: String, props: Vec<(&str, Value)>| Op::Set {
            target: Target::Id(id),
            props: props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        };
        let ops = match call.action.as_str() {
            "claim_open" => vec![Op::Create {
                reference: "claim".into(),
                kind: "claim".into(),
                props: [
                    ("card".to_string(), Value::from(text("card"))),
                    ("text".to_string(), Value::from(text("text"))),
                    ("verdict".to_string(), Value::from("open")),
                ]
                .into(),
            }],
            "claim_verdict" => vec![set(text("id"), vec![("verdict", text("verdict").into())])],
            "claim_rewrite" => {
                // A rewrite of a settled claim invalidates its verdict.
                let mut props = vec![("text", Value::from(text("text")))];
                if str_of(view, &text("id"), "verdict").is_some_and(|v| SETTLED.contains(&v)) {
                    props.push(("verdict", "stale".into()));
                }
                vec![set(text("id"), props)]
            }
            "claim_withdraw" => vec![set(text("id"), vec![("withdrawn", true.into())])],
            other => return Err(CapError::new("unknown_action", other)),
        };
        Ok(Plan {
            ops,
            readback: String::new(),
        })
    }
    fn validate(&self, _delta: &Delta, _view: &View) -> Result<(), CapError> {
        Ok(())
    }

    /// Fixture policy:
    /// - `accepted`/`rejected` verdicts are written only by humans;
    /// - `stale` only when the same change rewrites the text of a settled claim;
    /// - rewriting a settled claim must mark it `stale` in the same change;
    /// - `open` only on creation;
    /// - only the participant who opened a claim may withdraw it;
    /// - a settled claim is deleted only by a human.
    ///
    /// Every rule applies to what was written, including same-value writes.
    fn validate_change(&self, change: &Change<'_>) -> Result<(), CapError> {
        self.seen.lock().unwrap().push(Seen {
            principal: change.principal.clone(),
            origin: change.origin.clone(),
            attempted: change
                .attempted
                .iter()
                .map(|(id, keys)| (id.clone(), keys.iter().cloned().collect()))
                .collect(),
            delta: change.delta.clone(),
            before_ids: change.before.records().map(|r| r.id.clone()).collect(),
            after_ids: change.after.records().map(|r| r.id.clone()).collect(),
        });
        let human = change.principal.caller == CallerKind::Human;
        let refuse = |code: &str, message: String| Err(CapError::new(code, message));
        for (id, keys) in change.attempted {
            if change.after.get(id).map(|r| r.kind.as_str()) != Some("claim") {
                continue;
            }
            let created = change.delta.created.contains(id);
            let before_verdict = str_of(change.before, id, "verdict").unwrap_or_default();
            let text_changed = change
                .delta
                .modified
                .get(id)
                .is_some_and(|k| k.contains("text"));
            if keys.contains("verdict") {
                let verdict = str_of(change.after, id, "verdict").unwrap_or_default();
                match verdict {
                    "accepted" | "rejected" if !human => {
                        return refuse(
                            "human_verdict",
                            format!("{id}: only a human settles a claim"),
                        );
                    }
                    "accepted" | "rejected" => {}
                    "stale" if text_changed && SETTLED.contains(&before_verdict) => {}
                    "stale" => {
                        return refuse(
                            "invalid_transition",
                            format!("{id}: stale only when a settled claim is rewritten"),
                        );
                    }
                    "open" if created => {}
                    other => {
                        return refuse(
                            "invalid_transition",
                            format!("{id}: verdict {other:?} not allowed here"),
                        );
                    }
                }
            }
            if text_changed
                && SETTLED.contains(&before_verdict)
                && str_of(change.after, id, "verdict") != Some("stale")
            {
                return refuse(
                    "must_invalidate",
                    format!("{id}: rewriting a settled claim must mark it stale"),
                );
            }
            if keys.contains("withdrawn") && !created {
                let opener = str_of(change.before, id, "created_by_participant");
                if opener != Some(change.principal.participant.as_str()) {
                    return refuse(
                        "not_opener",
                        format!("{id}: only the participant who opened it may withdraw it"),
                    );
                }
            }
        }
        for id in &change.delta.deleted {
            let settled = change
                .before
                .get(id)
                .filter(|r| r.kind == "claim")
                .and_then(|r| r.props.get("verdict"))
                .and_then(|v| v.as_str())
                .is_some_and(|v| SETTLED.contains(&v));
            if settled && !human {
                return refuse(
                    "human_verdict",
                    format!("{id}: a settled claim is removed only by a human"),
                );
            }
        }
        Ok(())
    }
    /// Claims go with the card they are about.
    fn cascade(&self, deleted: &[String], view: &View) -> Result<Plan, CapError> {
        let ops = view
            .records()
            .filter(|r| r.kind == "claim")
            .filter(|r| {
                r.props
                    .get("card")
                    .and_then(|v| v.as_str())
                    .is_some_and(|c| deleted.iter().any(|d| d == c))
            })
            .map(|r| Op::Delete {
                target: Target::Id(r.id.clone()),
            })
            .collect();
        Ok(Plan {
            ops,
            readback: String::new(),
        })
    }
    fn digest(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
}

// ── fixture: evidence verified by native facts ───────────────────────────

struct Evidence;

impl Evidence {
    fn descriptor() -> Descriptor {
        Descriptor::new("evidence", "0.1.0", Artifact::Wasm)
            .depends_on("cards", "0.1.0", "evidence is about cards")
            .owns(
                "evidence",
                &["card", "path", "verified"],
                "evidence for a card",
            )
            .action(ActionDecl::new(
                "evidence_add",
                Audience::Both,
                Effect::Mutate,
                "Add evidence.",
            ))
    }
}

impl Capability for Evidence {
    fn descriptor(&self) -> Descriptor {
        Self::descriptor()
    }
    fn project(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
    fn plan(&self, call: &Call, _view: &View) -> Result<Plan, CapError> {
        // The plan claims `verified`; only the host's facts can back it.
        Ok(Plan {
            ops: vec![Op::Create {
                reference: "evidence".into(),
                kind: "evidence".into(),
                props: [
                    (
                        "card".to_string(),
                        Value::from(call.args["card"].as_str().unwrap_or_default()),
                    ),
                    (
                        "path".to_string(),
                        Value::from(call.args["path"].as_str().unwrap_or_default()),
                    ),
                    ("verified".to_string(), Value::from(true)),
                ]
                .into(),
            }],
            readback: String::new(),
        })
    }
    fn validate(&self, _delta: &Delta, _view: &View) -> Result<(), CapError> {
        Ok(())
    }
    /// Fixture policy: `verified = true` needs this host's facts to list the
    /// path as existing. No facts, no verification.
    fn validate_change(&self, change: &Change<'_>) -> Result<(), CapError> {
        for (id, keys) in change.attempted {
            let Some(record) = change.after.get(id).filter(|r| r.kind == "evidence") else {
                continue;
            };
            let claims_verified = record.props.get("verified") == Some(&Scalar::Bool(true));
            if !(claims_verified && (keys.contains("verified") || keys.contains("path"))) {
                continue;
            }
            let facts = change
                .facts
                .ok_or_else(|| CapError::new("no_facts", "no verifier established facts"))?;
            let path = record
                .props
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let exists = facts["existing"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|p| p == path);
            if !exists {
                return Err(CapError::new(
                    "unverified",
                    format!("{path:?} is not a file this host can verify"),
                ));
            }
        }
        Ok(())
    }
    fn digest(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
}

/// A bounded in-memory "repository": which paths exist.
struct Repository {
    files: BTreeSet<String>,
    calls: Arc<Mutex<usize>>,
}

impl Verifier for Repository {
    fn verify(&self, change: &Change<'_>) -> Result<serde_json::Value, CapError> {
        *self.calls.lock().unwrap() += 1;
        let mut existing = Vec::new();
        for record in change.after.records().filter(|r| r.kind == "evidence") {
            let path = record
                .props
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if path == "explode" {
                return Err(CapError::new(
                    "verifier_error",
                    "the repository could not be read",
                ));
            }
            if self.files.contains(path) {
                existing.push(path.to_string());
            }
        }
        Ok(json!({ "existing": existing }))
    }
}

// ── setup ────────────────────────────────────────────────────────────────

fn composition() -> Composition {
    Composition::new(vec![
        Cards::descriptor(),
        Claims::descriptor(),
        Evidence::descriptor(),
    ])
    .unwrap()
}

fn claims_host(claims: &Claims, verifier: Option<Arc<dyn Verifier>>) -> Arc<Host> {
    let host = Host::in_memory(
        composition(),
        vec![
            Arc::new(Cards),
            Arc::new(claims.clone()),
            Arc::new(Evidence),
        ],
    )
    .unwrap();
    let host = match verifier {
        Some(v) => host.with_verifier("evidence", v).unwrap(),
        None => host,
    };
    Arc::new(host)
}

fn repository() -> (Arc<dyn Verifier>, Arc<Mutex<usize>>) {
    let calls = Arc::new(Mutex::new(0));
    let repo = Repository {
        files: ["src/lib.rs".to_string()].into(),
        calls: calls.clone(),
    };
    (Arc::new(repo), calls)
}

/// Assert a refusal left state, revision, journal and callbacks unchanged.
fn unchanged<T: std::fmt::Debug>(host: &Arc<Host>, result: impl FnOnce() -> T) -> T {
    let before = fingerprint(host);
    let seen = record_commits(host);
    let result = result();
    assert_eq!(
        fingerprint(host),
        before,
        "a refusal changed the host: {result:?}"
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "a refusal announced a commit: {result:?}"
    );
    result
}

fn open_claim(host: &Host, card: &str, actor: &Actor) -> String {
    host.invoke(
        actor,
        "claim_open",
        json!({ "card": card, "text": "It works" }),
    )
    .unwrap()
    .ids["claim"]
        .clone()
}

fn set_batch(host: &Host, capability: &str, id: &str, props: Vec<(&str, Value)>) -> Batch {
    Batch {
        base_revision: host.revision(),
        entries: vec![BatchEntry {
            capability: capability.into(),
            plan: Plan {
                ops: vec![Op::Set {
                    target: Target::Id(id.into()),
                    props: props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
                }],
                readback: String::new(),
            },
        }],
    }
}

fn frame_setting(host: &Host, id: &str, key: &str, value: yrs::Any) -> Vec<u8> {
    let mut peer = Peer::of(host);
    {
        let objects = peer.objects();
        let mut txn = peer.doc.transact_mut();
        let Some(yrs::Out::YMap(record)) = objects.get(&txn, id) else {
            panic!("no {id}")
        };
        record.insert(&mut txn, key, value);
    }
    peer.frame()
}

fn code_of_validate(result: Result<impl std::fmt::Debug, CommitError>) -> String {
    match result {
        Err(CommitError::Validate { error, .. }) => error.code,
        other => panic!("expected a validation refusal, got {other:?}"),
    }
}

// ── tests ────────────────────────────────────────────────────────────────

#[test]
fn the_hook_receives_the_authenticated_principal_route_and_both_views() {
    let claims = Claims::default();
    let host = claims_host(&claims, None);
    let card = place(&host, "API");

    // Host action, called by its alias: the preferred name is recorded.
    let claim = host
        .invoke(
            &alice(),
            "atlas_claim",
            json!({ "card": card, "text": "It works" }),
        )
        .unwrap()
        .ids["claim"]
        .clone();
    let seen = claims.seen.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        seen.principal,
        Principal {
            participant: "p-alice".into(),
            caller: CallerKind::Human,
            label: Some("Alice".into())
        }
    );
    assert_eq!(
        seen.origin,
        Origin::HostAction {
            capability: "claims".into(),
            action: "claim_open".into()
        }
    );
    assert!(!seen.before_ids.contains(&claim) && seen.after_ids.contains(&claim));
    assert!(seen.delta.created.contains(&claim));
    assert!(
        seen.attempted
            .iter()
            .any(|(id, keys)| id == &claim && keys.contains(&"verdict".to_string()))
    );

    // Untrusted batch.
    host.commit_batch(
        &alice(),
        &set_batch(
            &host,
            "claims",
            &claim,
            vec![("verdict", "accepted".into())],
        ),
    )
    .unwrap();
    assert_eq!(
        claims.seen.lock().unwrap().last().unwrap().origin,
        Origin::UntrustedBatch
    );

    // Replica frame; no author is read from the frame.
    let frame = frame_setting(&host, &claim, "withdrawn", yrs::Any::Bool(true));
    host.receive_frame(&alice(), &frame).unwrap();
    let seen = claims.seen.lock().unwrap().last().unwrap().clone();
    assert_eq!(seen.origin, Origin::ReplicaFrame);
    assert_eq!(seen.principal.participant, "p-alice");
}

#[test]
fn only_a_human_settles_a_claim_on_every_route() {
    let claims = Claims::default();
    let host = claims_host(&claims, None);
    let card = place(&host, "API");
    let claim = open_claim(&host, &card, &agent());

    // The action's audience already stops an agent...
    assert!(matches!(
        host.invoke(
            &agent(),
            "claim_verdict",
            json!({ "id": claim, "verdict": "accepted" })
        ),
        Err(InvokeError::Audience { .. })
    ));
    // ...but a batch and a frame do not go through that action. The policy
    // does.
    let batch = set_batch(
        &host,
        "claims",
        &claim,
        vec![("verdict", "accepted".into())],
    );
    let code = unchanged(&host, || {
        code_of_validate(host.commit_batch(&agent(), &batch))
    });
    assert_eq!(code, "human_verdict");
    let frame = frame_setting(&host, &claim, "verdict", "accepted".into());
    let refusal = unchanged(&host, || host.receive_frame(&agent(), &frame).unwrap_err());
    assert!(
        matches!(refusal, FrameRefusal::Validate { ref error, .. } if error.code == "human_verdict"),
        "{refusal}"
    );

    // A human may, by action, batch or frame.
    host.invoke(
        &alice(),
        "claim_verdict",
        json!({ "id": claim, "verdict": "accepted" }),
    )
    .unwrap();
    assert_eq!(str_of(&host.view(), &claim, "verdict"), Some("accepted"));
}

#[test]
fn an_agent_rewrite_invalidates_a_human_verdict_and_cannot_skip_the_invalidation() {
    let claims = Claims::default();
    let host = claims_host(&claims, None);
    let card = place(&host, "API");
    let claim = open_claim(&host, &card, &agent());
    host.invoke(
        &alice(),
        "claim_verdict",
        json!({ "id": claim, "verdict": "accepted" }),
    )
    .unwrap();

    // A rewrite that leaves the verdict standing is refused, even from a batch.
    let silent = set_batch(
        &host,
        "claims",
        &claim,
        vec![("text", "It works differently".into())],
    );
    let code = unchanged(&host, || {
        code_of_validate(host.commit_batch(&agent(), &silent))
    });
    assert_eq!(code, "must_invalidate");

    // The legitimate conditional transition: rewrite and mark stale together.
    host.invoke(
        &agent(),
        "claim_rewrite",
        json!({ "id": claim, "text": "It works differently" }),
    )
    .unwrap();
    let view = host.view();
    assert_eq!(str_of(&view, &claim, "verdict"), Some("stale"));
    assert_eq!(
        view.get(&claim).unwrap().props["touched_by_participant"],
        Scalar::from("p-agent")
    );

    // Marking stale without a rewrite is refused.
    let mut claim_two = open_claim(&host, &card, &agent());
    host.invoke(
        &alice(),
        "claim_verdict",
        json!({ "id": claim_two, "verdict": "rejected" }),
    )
    .unwrap();
    let stale_only = set_batch(
        &host,
        "claims",
        &claim_two,
        vec![("verdict", "stale".into())],
    );
    let code = unchanged(&host, || {
        code_of_validate(host.commit_batch(&agent(), &stale_only))
    });
    assert_eq!(code, "invalid_transition");
    claim_two.clear();
}

#[test]
fn a_same_value_write_is_authorized_like_any_other_write() {
    let claims = Claims::default();
    let host = claims_host(&claims, None);
    let card = place(&host, "API");
    let claim = open_claim(&host, &card, &agent());
    host.invoke(
        &alice(),
        "claim_verdict",
        json!({ "id": claim, "verdict": "accepted" }),
    )
    .unwrap();

    // The agent rewrites "accepted" with "accepted": no value changes, the
    // write is still an attempt to settle the claim.
    let batch = set_batch(
        &host,
        "claims",
        &claim,
        vec![("verdict", "accepted".into())],
    );
    let code = unchanged(&host, || {
        code_of_validate(host.commit_batch(&agent(), &batch))
    });
    assert_eq!(code, "human_verdict");
    let frame = frame_setting(&host, &claim, "verdict", "accepted".into());
    let refusal = unchanged(&host, || host.receive_frame(&agent(), &frame).unwrap_err());
    assert!(
        matches!(refusal, FrameRefusal::Validate { ref error, .. } if error.code == "human_verdict"),
        "{refusal}"
    );
    let seen = claims.seen.lock().unwrap().last().unwrap().clone();
    assert!(seen.delta.is_empty(), "the refused frame changed no value");
    assert!(
        seen.attempted
            .iter()
            .any(|(id, keys)| id == &claim && keys.contains(&"verdict".to_string()))
    );

    // The same write by a human is allowed; nothing visible changes.
    let reply = host
        .receive_frame(
            &alice(),
            &frame_setting(&host, &claim, "verdict", "accepted".into()),
        )
        .unwrap();
    assert!(matches!(reply.outcome, FrameOutcome::Committed { .. }));
    assert!(!host.journal().recent.back().unwrap().visible_change);
}

#[test]
fn participants_with_the_same_display_name_are_told_apart() {
    let claims = Claims::default();
    let host = claims_host(&claims, None);
    let card = place(&host, "API");
    let first = Actor::new("p-1", Caller::Human).labelled("Alice");
    let second = Actor::new("p-2", Caller::Human).labelled("Alice");
    let claim = open_claim(&host, &card, &first);

    let code = unchanged(&host, || {
        match host.invoke(&second, "claim_withdraw", json!({ "id": claim })) {
            Err(InvokeError::Commit(CommitError::Validate { error, .. })) => error.code,
            other => panic!("{other:?}"),
        }
    });
    assert_eq!(code, "not_opener");
    host.invoke(&first, "claim_withdraw", json!({ "id": claim }))
        .unwrap();
    assert_eq!(
        host.view().get(&claim).unwrap().props["withdrawn"],
        Scalar::Bool(true)
    );
}

#[test]
fn a_cascaded_deletion_is_authorized_with_the_original_principal_and_the_before_records() {
    let claims = Claims::default();
    let host = claims_host(&claims, None);
    let card = place(&host, "API");
    let claim = open_claim(&host, &card, &agent());
    host.invoke(
        &alice(),
        "claim_verdict",
        json!({ "id": claim, "verdict": "accepted" }),
    )
    .unwrap();

    // The agent removes the card; the cascade would delete a settled claim.
    let result = unchanged(&host, || {
        host.invoke(&agent(), "object_remove", json!({ "id": card }))
    });
    assert!(
        matches!(result, Err(InvokeError::Commit(CommitError::Validate { ref error, .. })) if error.code == "human_verdict"),
        "{result:?}"
    );
    let seen = claims.seen.lock().unwrap().last().unwrap().clone();
    assert_eq!(seen.principal.participant, "p-agent");
    assert_eq!(
        seen.origin,
        Origin::HostAction {
            capability: "cards".into(),
            action: "object_remove".into()
        }
    );
    assert!(seen.delta.deleted.contains(&claim) && seen.before_ids.contains(&claim));

    // A human removal takes the claim with the card in one commit.
    let commit = host
        .invoke(&alice(), "object_remove", json!({ "id": card }))
        .unwrap()
        .commit
        .unwrap();
    assert!(commit.deleted.contains(&claim) && commit.deleted.contains(&card));
}

#[test]
fn a_drained_held_frame_is_authorized_as_its_original_actor_and_route() {
    let claims = Claims::default();
    let host = claims_host(&claims, None);
    let card = place(&host, "API");
    let claim = open_claim(&host, &card, &alice());

    // Bob's unsent card, and Alice's withdrawal that depends on it.
    let mut bob_peer = Peer::of(&host);
    let bob_card = format!("{:016x}-{:08x}", bob_peer.client(), 0);
    {
        let objects = bob_peer.objects();
        let mut txn = bob_peer.doc.transact_mut();
        let new = objects.insert(&mut txn, bob_card.as_str(), MapPrelim::default());
        new.insert(&mut txn, "kind", "card");
        new.insert(&mut txn, "label", "Bob's");
    }
    let bob_frame = bob_peer.frame();
    let mut alice_peer = Peer::of(&host);
    alice_peer.apply_frame(&bob_frame);
    let _ = alice_peer.frame();
    {
        let objects = alice_peer.objects();
        let mut txn = alice_peer.doc.transact_mut();
        let Some(yrs::Out::YMap(new)) = objects.get(&txn, &bob_card) else {
            panic!()
        };
        new.insert(&mut txn, "x", 5.0);
        let Some(yrs::Out::YMap(record)) = objects.get(&txn, &claim) else {
            panic!()
        };
        record.insert(&mut txn, "withdrawn", true);
    }
    let alice_frame = alice_peer.frame();
    assert!(matches!(
        host.receive_frame(&alice(), &alice_frame).unwrap().outcome,
        FrameOutcome::Held { .. }
    ));
    host.receive_frame(&bob(), &bob_frame).unwrap();
    assert!(host.held().is_empty());

    let seen = claims.seen.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        seen.principal.participant, "p-alice",
        "the drained change is Alice's"
    );
    assert_eq!(seen.origin, Origin::ReplicaFrame);
    assert_eq!(
        host.view().get(&claim).unwrap().props["withdrawn"],
        Scalar::Bool(true)
    );
}

#[test]
fn verification_needs_native_facts_and_cannot_be_forged() {
    let claims = Claims::default();

    // No verifier registered: evidence cannot be marked verified at all.
    let host = claims_host(&claims, None);
    let card = place(&host, "API");
    let result = unchanged(&host, || {
        host.invoke(
            &agent(),
            "evidence_add",
            json!({ "card": card, "path": "src/lib.rs" }),
        )
    });
    assert!(
        matches!(result, Err(InvokeError::Commit(CommitError::Validate { ref error, .. })) if error.code == "no_facts"),
        "{result:?}"
    );

    // With the host's verifier: a real file verifies; a missing one does not,
    // whatever the plan, batch or arguments claim.
    let (repo, calls) = repository();
    let host = claims_host(&claims, Some(repo));
    let card = place(&host, "API");
    host.invoke(
        &agent(),
        "evidence_add",
        json!({ "card": card, "path": "src/lib.rs" }),
    )
    .unwrap();
    assert!(*calls.lock().unwrap() >= 1);
    let result = unchanged(&host, || {
        host.invoke(
            &agent(),
            "evidence_add",
            json!({ "card": card, "path": "missing.rs", "verified": true }),
        )
    });
    assert!(
        matches!(result, Err(InvokeError::Commit(CommitError::Validate { ref error, .. })) if error.code == "unverified"),
        "{result:?}"
    );
    let forged = Batch {
        base_revision: host.revision(),
        entries: vec![BatchEntry {
            capability: "evidence".into(),
            plan: Plan {
                ops: vec![Op::Create {
                    reference: "e".into(),
                    kind: "evidence".into(),
                    props: [
                        ("card".to_string(), Value::from(card.as_str())),
                        ("path".to_string(), Value::from("missing.rs")),
                        ("verified".to_string(), Value::from(true)),
                    ]
                    .into(),
                }],
                readback: String::new(),
            },
        }],
    };
    let code = unchanged(&host, || {
        code_of_validate(host.commit_batch(&alice(), &forged))
    });
    assert_eq!(code, "unverified");

    // A verifier failure refuses with its own error.
    let exploding = Batch {
        base_revision: host.revision(),
        entries: vec![BatchEntry {
            capability: "evidence".into(),
            plan: Plan {
                ops: vec![Op::Create {
                    reference: "e".into(),
                    kind: "evidence".into(),
                    props: [
                        ("card".to_string(), Value::from(card.as_str())),
                        ("path".to_string(), Value::from("explode")),
                        ("verified".to_string(), Value::from(false)),
                    ]
                    .into(),
                }],
                readback: String::new(),
            },
        }],
    };
    let result = unchanged(&host, || host.commit_batch(&agent(), &exploding));
    assert!(
        matches!(result, Err(CommitError::Verify { ref capability, ref error }) if capability == "evidence" && error.code == "verifier_error"),
        "{result:?}"
    );
}

#[test]
fn a_verifier_is_registered_once_for_a_composed_capability() {
    let (repo, _) = repository();
    let host = Host::in_memory(
        composition(),
        vec![
            Arc::new(Cards),
            Arc::new(Claims::default()),
            Arc::new(Evidence),
        ],
    )
    .unwrap();
    assert!(host.with_verifier("nothing", repo.clone()).is_err());
    let host = Host::in_memory(
        composition(),
        vec![
            Arc::new(Cards),
            Arc::new(Claims::default()),
            Arc::new(Evidence),
        ],
    )
    .unwrap();
    let host = host.with_verifier("evidence", repo.clone()).unwrap();
    assert!(host.with_verifier("evidence", repo).is_err());
}
