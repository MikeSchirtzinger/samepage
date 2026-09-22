#![allow(dead_code)]

//! Test capabilities and peers. These are real `Capability` implementations
//! driven through the real kernel; they prove the kernel's mechanics, not any
//! product capability.

use std::sync::{Arc, Mutex};

use samepage_capability::{
    ActionDecl, Artifact, Audience, Call, CapError, Capability, Composition, Delta, Descriptor,
    Effect, Op, Plan, Target, Value, View,
};
use samepage_doc::host::{Commit, Host};
use samepage_doc::{sync, Actor, Caller};
use serde_json::json;
use yrs::updates::decoder::Decode;
use yrs::{Doc, ReadTxn, StateVector, Transact, Update};

pub struct Cards;

impl Cards {
    pub fn descriptor() -> Descriptor {
        Descriptor::new("cards", "0.1.0", Artifact::Wasm)
            .owns("card", &["label", "x", "y", "note", "parent"], "cards")
            .owns("link", &["from", "to"], "links between cards")
            .action(
                ActionDecl::new(
                    "card_place",
                    Audience::Both,
                    Effect::Mutate,
                    "Place a card.",
                )
                .alias("atlas_place"),
            )
            .action(ActionDecl::new(
                "card_move",
                Audience::Both,
                Effect::Mutate,
                "Move a card.",
            ))
            .action(ActionDecl::new(
                "card_link",
                Audience::Both,
                Effect::Mutate,
                "Link two cards.",
            ))
            .action(ActionDecl::new(
                "object_remove",
                Audience::Both,
                Effect::Mutate,
                "Remove.",
            ))
            .action(ActionDecl::new(
                "page_count",
                Audience::Both,
                Effect::Query,
                "Count cards.",
            ))
            .action(ActionDecl::new(
                "page_sneak",
                Audience::Both,
                Effect::Query,
                "A query that wrongly plans a write.",
            ))
            .action(ActionDecl::new(
                "card_tidy",
                Audience::Both,
                Effect::Mutate,
                "A mutating action that finds nothing to change.",
            ))
            .action(ActionDecl::new(
                "card_seal",
                Audience::Human,
                Effect::Mutate,
                "Human-only: seal a card.",
            ))
    }
}

impl Capability for Cards {
    fn descriptor(&self) -> Descriptor {
        Self::descriptor()
    }

    fn project(&self, view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({ "cards": view.of_kinds(&["card"]).len() }))
    }

    fn plan(&self, call: &Call, view: &View) -> Result<Plan, CapError> {
        let args = &call.args;
        let text = |key: &str| args[key].as_str().unwrap_or_default().to_string();
        let ops = match call.action.as_str() {
            "card_place" => vec![Op::Create {
                reference: "card".into(),
                kind: "card".into(),
                props: [
                    ("label".to_string(), Value::from(text("label"))),
                    (
                        "x".to_string(),
                        Value::from(args["x"].as_f64().unwrap_or(0.0)),
                    ),
                ]
                .into(),
            }],
            "card_move" => vec![Op::Set {
                target: Target::Id(text("id")),
                props: [(
                    "x".to_string(),
                    Value::from(args["x"].as_f64().unwrap_or(0.0)),
                )]
                .into(),
            }],
            "card_seal" => vec![Op::Set {
                target: Target::Id(text("id")),
                props: [("note".to_string(), Value::from("sealed"))].into(),
            }],
            "card_link" => vec![Op::Create {
                reference: "link".into(),
                kind: "link".into(),
                props: [
                    ("from".to_string(), Value::from(text("from"))),
                    ("to".to_string(), Value::from(text("to"))),
                ]
                .into(),
            }],
            "object_remove" => vec![Op::Delete {
                target: Target::Id(text("id")),
            }],
            "page_count" => {
                return Ok(Plan {
                    ops: vec![],
                    readback: format!("{} cards", view.of_kinds(&["card"]).len()),
                })
            }
            "card_tidy" => {
                return Ok(Plan {
                    ops: vec![],
                    readback: "Nothing to tidy.".into(),
                })
            }
            "page_sneak" => vec![Op::Set {
                target: Target::Id(text("id")),
                props: [("x".to_string(), Value::from(1.0))].into(),
            }],
            other => return Err(CapError::new("unknown_action", other)),
        };
        Ok(Plan {
            ops,
            readback: String::new(),
        })
    }

    fn validate(&self, delta: &Delta, view: &View) -> Result<(), CapError> {
        for id in delta.created.iter().chain(delta.modified.keys()) {
            if let Some(record) = view.get(id) {
                if record.kind == "card"
                    && record.props.get("label").and_then(|v| v.as_str()) == Some("")
                {
                    return Err(CapError::new(
                        "empty_label",
                        format!("{id} has an empty label"),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Links to a removed card go with it.
    fn cascade(&self, deleted: &[String], view: &View) -> Result<Plan, CapError> {
        let ops = view
            .records()
            .filter(|record| record.kind == "link")
            .filter(|record| {
                ["from", "to"].iter().any(|key| {
                    record
                        .props
                        .get(*key)
                        .and_then(|v| v.as_str())
                        .is_some_and(|id| deleted.iter().any(|d| d == id))
                })
            })
            .map(|record| Op::Delete {
                target: Target::Id(record.id.clone()),
            })
            .collect();
        Ok(Plan {
            ops,
            readback: String::new(),
        })
    }

    fn digest(&self, view: &View) -> Result<serde_json::Value, CapError> {
        self.project(view)
    }
}

pub struct Sources;

impl Sources {
    pub fn descriptor() -> Descriptor {
        Descriptor::new("sources", "0.1.0", Artifact::Wasm)
            .depends_on("cards", "0.1.0", "binds cards to files")
            .extends("card", &["path"], "source binding")
            .action(ActionDecl::new(
                "source_bind",
                Audience::Agent,
                Effect::Mutate,
                "Bind.",
            ))
    }
}

impl Capability for Sources {
    fn descriptor(&self) -> Descriptor {
        Self::descriptor()
    }
    fn project(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
    fn plan(&self, call: &Call, _view: &View) -> Result<Plan, CapError> {
        Ok(Plan {
            ops: vec![Op::Set {
                target: Target::Id(call.args["id"].as_str().unwrap_or_default().to_string()),
                props: [(
                    "path".to_string(),
                    Value::from(call.args["path"].as_str().unwrap_or_default()),
                )]
                .into(),
            }],
            readback: String::new(),
        })
    }
    fn validate(&self, _delta: &Delta, _view: &View) -> Result<(), CapError> {
        Ok(())
    }
    /// Fixture policy: a source binding is written only by an agent, on any
    /// route. `source_bind` being Agent-only does not imply this; the policy
    /// is stated here.
    fn validate_change(&self, change: &samepage_capability::Change<'_>) -> Result<(), CapError> {
        let writes_path = change.attempted.values().any(|keys| keys.contains("path"));
        if writes_path && change.principal.caller != samepage_capability::CallerKind::Agent {
            return Err(CapError::new(
                "agent_only",
                "a source binding is written by an agent",
            ));
        }
        Ok(())
    }
    fn digest(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
}

pub fn composition() -> Composition {
    Composition::new(vec![Cards::descriptor(), Sources::descriptor()]).unwrap()
}

pub fn capabilities() -> Vec<Arc<dyn Capability>> {
    vec![Arc::new(Cards), Arc::new(Sources)]
}

pub fn host() -> Host {
    Host::in_memory(composition(), capabilities()).unwrap()
}

pub fn alice() -> Actor {
    Actor::new("p-alice", Caller::Human).labelled("Alice")
}

pub fn bob() -> Actor {
    Actor::new("p-bob", Caller::Human).labelled("Bob")
}

pub fn agent() -> Actor {
    Actor::new("p-agent", Caller::Agent).labelled("Model")
}

/// Every commit an observer saw, with the host's view at that moment.
pub fn record_commits(host: &Arc<Host>) -> Arc<Mutex<Vec<(Commit, View)>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let weak = Arc::downgrade(host);
    host.on_commit(move |commit| {
        let view = weak.upgrade().map(|host| host.view());
        if let Some(view) = view {
            sink.lock().unwrap().push((commit.clone(), view));
        }
    });
    seen
}

/// A raw Yrs peer holding the host's state: used to craft updates an honest
/// replica would never send.
pub struct Peer {
    pub doc: Doc,
    mark: StateVector,
}

impl Peer {
    pub fn of(host: &Host) -> Self {
        Self::with_doc(Doc::new(), host)
    }

    pub fn with_client(client: u64, host: &Host) -> Self {
        Self::with_doc(Doc::with_client_id(client), host)
    }

    fn with_doc(doc: Doc, host: &Host) -> Self {
        doc.get_or_insert_map("objects");
        doc.transact_mut()
            .apply_update(Update::decode_v1(&host.encode_full()).unwrap())
            .unwrap();
        let mark = doc.transact().state_vector();
        Self { doc, mark }
    }

    pub fn objects(&self) -> yrs::MapRef {
        self.doc.get_or_insert_map("objects")
    }

    pub fn client(&self) -> u64 {
        self.doc.client_id().get()
    }

    /// Everything written since the last call, as one Update frame.
    pub fn frame(&mut self) -> Vec<u8> {
        let update = self.doc.transact().encode_state_as_update_v1(&self.mark);
        self.mark = self.doc.transact().state_vector();
        sync::update(update)
    }

    /// Raw update bytes since `sv` (for reordering tests).
    pub fn since(&self, sv: &StateVector) -> Vec<u8> {
        self.doc.transact().encode_state_as_update_v1(sv)
    }

    pub fn sv(&self) -> StateVector {
        self.doc.transact().state_vector()
    }

    pub fn apply_frame(&self, frame: &[u8]) {
        for message in sync::parse(frame).unwrap() {
            if let sync::Incoming::Update(update) = message {
                self.doc
                    .transact_mut()
                    .apply_update(Update::decode_v1(&update).unwrap())
                    .unwrap();
            }
        }
    }
}

/// Everything observable about the host that a refusal must leave unchanged.
#[derive(Debug, PartialEq)]
pub struct Fingerprint {
    pub revision: String,
    pub state: Vec<u8>,
    pub seq: u64,
    pub bindings: usize,
    pub held: std::collections::BTreeMap<String, usize>,
}

pub fn fingerprint(host: &Host) -> Fingerprint {
    let journal = host.journal();
    Fingerprint {
        revision: host.revision(),
        state: host.encode_full(),
        seq: journal.seq,
        bindings: journal.bindings.len(),
        held: host.held(),
    }
}

/// Place a card as the agent and return its id.
pub fn place(host: &Host, label: &str) -> String {
    let invoked = host
        .invoke(&agent(), "card_place", json!({ "label": label, "x": 1.0 }))
        .unwrap();
    invoked.ids["card"].clone()
}
