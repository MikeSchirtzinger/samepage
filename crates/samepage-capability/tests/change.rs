//! The host authority hook's types and its default.

use std::collections::{BTreeMap, BTreeSet};

use samepage_capability::{
    Artifact, Call, CallerKind, CapError, Capability, Change, Delta, Descriptor, Origin, Plan,
    Principal, View,
};
use serde_json::json;

/// Refuses every structural check.
struct Strict;

impl Capability for Strict {
    fn descriptor(&self) -> Descriptor {
        Descriptor::new("strict", "0.1.0", Artifact::Wasm)
    }
    fn project(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
    fn plan(&self, _call: &Call, _view: &View) -> Result<Plan, CapError> {
        Ok(Plan::default())
    }
    fn validate(&self, _delta: &Delta, _view: &View) -> Result<(), CapError> {
        Err(CapError::new("structural", "refused"))
    }
    fn digest(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
}

#[test]
fn the_default_hook_applies_the_structural_validate() {
    let view = View::new("r", vec![]).unwrap();
    let principal = Principal {
        participant: "p".into(),
        caller: CallerKind::Human,
        label: None,
    };
    let attempted: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let change = Change {
        principal: &principal,
        origin: &Origin::UntrustedBatch,
        before: &view,
        after: &view,
        delta: &Delta::default(),
        attempted: &attempted,
        facts: None,
    };
    assert_eq!(
        Strict.validate_change(&change).unwrap_err().code,
        "structural"
    );
}

#[test]
fn principal_and_origin_have_one_readable_json_form() {
    let principal = Principal {
        participant: "p-1".into(),
        caller: CallerKind::Agent,
        label: Some("Model".into()),
    };
    assert_eq!(
        serde_json::to_value(&principal).unwrap(),
        json!({ "participant": "p-1", "caller": "agent", "label": "Model" })
    );
    assert_eq!(
        serde_json::to_value(Origin::HostAction {
            capability: "map".into(),
            action: "card_place".into()
        })
        .unwrap(),
        json!({ "route": "host_action", "capability": "map", "action": "card_place" })
    );
    assert_eq!(
        serde_json::to_value(Origin::UntrustedBatch).unwrap(),
        json!({ "route": "untrusted_batch" })
    );
    assert_eq!(
        serde_json::to_value(Origin::ReplicaFrame).unwrap(),
        json!({ "route": "replica_frame" })
    );
}
