mod common;

use common::{map, sketch, sources};
use samepage_capability::{
    ActionDecl, Artifact, Audience, ComposeError, Composition, Descriptor, Effect,
};

fn refuse(descriptors: Vec<Descriptor>) -> ComposeError {
    Composition::new(descriptors).expect_err("composition must be refused")
}

#[test]
fn a_valid_set_resolves_owners_writers_and_names() {
    let composition = common::composition();
    assert_eq!(composition.owner_of_kind("node"), Some("map"));
    assert_eq!(composition.owner_of_kind("shape"), Some("sketch"));
    assert_eq!(composition.owner_of_kind("agent-ink-variable"), None);
    assert_eq!(composition.writer_of("node", "label"), Some("map"));
    assert_eq!(composition.writer_of("node", "path"), Some("sources"));
    assert_eq!(composition.writer_of("node", "state_variable"), None);
    assert!(composition.may_write("sources", "node", "lines"));
    assert!(!composition.may_write("map", "node", "lines"));

    let alias = composition
        .resolve_action("atlas_place")
        .expect("alias resolves");
    assert_eq!(
        (alias.capability, alias.preferred, alias.alias),
        ("map", "card_place", true)
    );
    let preferred = composition
        .resolve_action("card_place")
        .expect("name resolves");
    assert!(!preferred.alias);
    let names = composition.public_action_names();
    assert!(names.contains(&"card_place") && names.contains(&"source_read"));
    assert!(names
        .iter()
        .all(|name| !name.starts_with("atlas_") && *name != "repo_read"));
}

#[test]
fn kind_claims_have_no_wildcard() {
    // `map` owns `node` but only its listed keys; an unlisted key has no writer.
    let composition = common::composition();
    assert_eq!(composition.writer_of("node", "colour"), None);
}

#[test]
fn abi_mismatch_is_refused() {
    let mut old = map();
    old.abi = "samepage.capability.v0".into();
    assert!(matches!(
        refuse(vec![old]),
        ComposeError::AbiMismatch { .. }
    ));
}

#[test]
fn malformed_id_and_empty_version_are_refused() {
    let mut bad = map();
    bad.id = "Map_1".into();
    assert!(matches!(refuse(vec![bad]), ComposeError::InvalidId(_)));
    let mut unversioned = map();
    unversioned.version.clear();
    assert!(matches!(
        refuse(vec![unversioned]),
        ComposeError::EmptyVersion(_)
    ));
}

#[test]
fn duplicate_capability_is_refused() {
    assert!(matches!(
        refuse(vec![map(), map()]),
        ComposeError::DuplicateCapability(id) if id == "map"
    ));
}

#[test]
fn a_kind_has_one_owner() {
    let rival = Descriptor::new("rival", "0.1.0", Artifact::Wasm).owns("node", &["other"], "x");
    assert!(matches!(
        refuse(vec![map(), rival]),
        ComposeError::DuplicateKindOwner { kind, .. } if kind == "node"
    ));
}

#[test]
fn malformed_kind_and_key_are_refused() {
    let bad_kind = Descriptor::new("bad", "0.1.0", Artifact::Wasm).owns("Node", &["x"], "x");
    assert!(matches!(
        refuse(vec![bad_kind]),
        ComposeError::InvalidKind { .. }
    ));
    let bad_key = Descriptor::new("bad", "0.1.0", Artifact::Wasm).owns("thing", &["X-pos"], "x");
    assert!(matches!(
        refuse(vec![bad_key]),
        ComposeError::InvalidKey { .. }
    ));
}

#[test]
fn structural_authorship_and_underscore_keys_cannot_be_claimed() {
    for key in [
        "kind",
        "created_by",
        "touched_by",
        "created_by_participant",
        "touched_by_participant",
        "_rev",
    ] {
        let claim = Descriptor::new("bad", "0.1.0", Artifact::Wasm).owns("thing", &[key], "x");
        assert!(
            matches!(refuse(vec![claim]), ComposeError::ReservedKey { key: k, .. } if k == key),
            "{key} must be reserved"
        );
    }
    let extension = Descriptor::new("bad", "0.1.0", Artifact::Wasm)
        .depends_on("map", "0.1.0", "x")
        .extends("node", &["touched_by"], "x");
    assert!(matches!(
        refuse(vec![map(), extension]),
        ComposeError::ReservedKey { .. }
    ));
}

#[test]
fn a_key_has_one_writer() {
    let within = Descriptor::new("bad", "0.1.0", Artifact::Wasm).owns("thing", &["x", "x"], "x");
    assert!(matches!(
        refuse(vec![within]),
        ComposeError::DuplicateKey { .. }
    ));
    let over_owner = Descriptor::new("bad", "0.1.0", Artifact::Wasm)
        .depends_on("map", "0.1.0", "x")
        .extends("node", &["label"], "x");
    assert!(matches!(
        refuse(vec![map(), over_owner]),
        ComposeError::DuplicateKey { key, first, .. } if key == "label" && first == "map"
    ));
    let second_extension = Descriptor::new("other", "0.1.0", Artifact::Wasm)
        .depends_on("map", "0.1.0", "x")
        .extends("node", &["path"], "x");
    assert!(matches!(
        refuse(vec![map(), sources(), second_extension]),
        ComposeError::DuplicateKey { key, first, .. } if key == "path" && first == "sources"
    ));
}

#[test]
fn a_prop_claim_needs_an_owner_and_a_dependency_on_it() {
    let orphan = Descriptor::new("bad", "0.1.0", Artifact::Wasm).extends("ghost", &["x"], "x");
    assert!(matches!(
        refuse(vec![orphan]),
        ComposeError::PropClaimWithoutOwner { .. }
    ));

    let undeclared =
        Descriptor::new("bad", "0.1.0", Artifact::Wasm).extends("node", &["path"], "x");
    assert!(matches!(
        refuse(vec![map(), undeclared]),
        ComposeError::PropClaimWithoutDependency { owner, .. } if owner == "map"
    ));

    let own = Descriptor::new("bad", "0.1.0", Artifact::Wasm)
        .owns("thing", &["x"], "x")
        .extends("thing", &["y"], "x");
    assert!(matches!(
        refuse(vec![own]),
        ComposeError::PropClaimOnOwnKind { .. }
    ));

    // A transitive dependency on the owner is enough.
    let transitive = Descriptor::new("lanes", "0.1.0", Artifact::Wasm)
        .depends_on("sources", "0.1.0", "claims lanes to bound cards")
        .extends("node", &["lane_ids"], "lanes a card vouches for");
    Composition::new(vec![map(), sources(), transitive]).expect("transitive dependency accepted");
}

#[test]
fn reads_must_be_owned_by_a_dependency() {
    let stray = Descriptor::new("bad", "0.1.0", Artifact::Wasm).reads("node");
    assert!(matches!(
        refuse(vec![map(), stray]),
        ComposeError::ReadWithoutDependency { .. }
    ));
}

#[test]
fn dependencies_must_exist_match_and_come_first() {
    assert!(matches!(
        refuse(vec![sources()]),
        ComposeError::MissingDependency { dependency, .. } if dependency == "map"
    ));
    assert!(matches!(
        refuse(vec![sources(), map()]),
        ComposeError::DependencyOrder { dependency, .. } if dependency == "map"
    ));
    let mut newer = map();
    newer.version = "0.2.0".into();
    assert!(matches!(
        refuse(vec![newer, sources()]),
        ComposeError::DependencyVersion { wanted, found, .. } if wanted == "0.1.0" && found == "0.2.0"
    ));
    let selfish = Descriptor::new("loop", "0.1.0", Artifact::Wasm).depends_on("loop", "0.1.0", "x");
    assert!(matches!(
        refuse(vec![selfish]),
        ComposeError::SelfDependency(_)
    ));
    // A cycle cannot be written in dependency order: one side is always "later".
    let a = Descriptor::new("a", "0.1.0", Artifact::Wasm).depends_on("b", "0.1.0", "x");
    let b = Descriptor::new("b", "0.1.0", Artifact::Wasm).depends_on("a", "0.1.0", "x");
    assert!(matches!(
        refuse(vec![a, b]),
        ComposeError::DependencyOrder { .. }
    ));
}

#[test]
fn action_names_are_noun_verb_without_product_prefix() {
    let camel = map().action(ActionDecl::new(
        "placeCard",
        Audience::Both,
        Effect::Mutate,
        "x",
    ));
    assert!(matches!(
        refuse(vec![camel]),
        ComposeError::InvalidActionName { .. }
    ));
    let prefixed = map().action(ActionDecl::new(
        "atlas_draw",
        Audience::Both,
        Effect::Mutate,
        "x",
    ));
    assert!(matches!(
        refuse(vec![prefixed]),
        ComposeError::ProductPrefix { .. }
    ));
    let room = map().action(ActionDecl::new(
        "room_arrange",
        Audience::Both,
        Effect::Mutate,
        "x",
    ));
    assert!(matches!(
        refuse(vec![room]),
        ComposeError::ProductPrefix { .. }
    ));
    let bad_alias = map()
        .action(ActionDecl::new("draw", Audience::Both, Effect::Mutate, "x").alias("Atlas-Draw"));
    assert!(matches!(
        refuse(vec![bad_alias]),
        ComposeError::InvalidActionName { .. }
    ));
}

#[test]
fn every_name_and_alias_is_unique_across_the_set() {
    let clash = Descriptor::new("clash", "0.1.0", Artifact::Wasm).action(ActionDecl::new(
        "card_place",
        Audience::Both,
        Effect::Mutate,
        "x",
    ));
    assert!(matches!(
        refuse(vec![map(), clash]),
        ComposeError::DuplicateActionName { .. }
    ));
    let alias_clash = Descriptor::new("clash", "0.1.0", Artifact::Wasm)
        .action(ActionDecl::new("draw", Audience::Both, Effect::Mutate, "x").alias("atlas_read"));
    assert!(matches!(
        refuse(vec![map(), alias_clash]),
        ComposeError::DuplicateActionName { name, .. } if name == "atlas_read"
    ));
    let alias_is_a_name = Descriptor::new("clash", "0.1.0", Artifact::Wasm)
        .action(ActionDecl::new("draw", Audience::Both, Effect::Mutate, "x").alias("page_read"));
    assert!(matches!(
        refuse(vec![map(), alias_is_a_name]),
        ComposeError::DuplicateActionName { .. }
    ));
}

#[test]
fn native_only_is_for_adapters_that_own_no_document() {
    let adapter = Descriptor::new(
        "g8-adapter",
        "0.1.0",
        Artifact::NativeOnly {
            reason: "runs the g8 command-line tool".into(),
        },
    );
    Composition::new(vec![adapter]).expect("a host adapter owning nothing is allowed");

    let mut loophole = sources();
    loophole.artifact = Artifact::NativeOnly {
        reason: "browser uses host actions".into(),
    };
    assert!(matches!(
        refuse(vec![map(), loophole]),
        ComposeError::NativeOnlyClaimsDocument(id) if id == "sources"
    ));
    let owning = Descriptor::new("x", "0.1.0", Artifact::NativeOnly { reason: "r".into() }).owns(
        "thing",
        &["a"],
        "x",
    );
    assert!(matches!(
        refuse(vec![owning]),
        ComposeError::NativeOnlyClaimsDocument(_)
    ));

    let silent = Descriptor::new("x", "0.1.0", Artifact::NativeOnly { reason: " ".into() });
    assert!(matches!(
        refuse(vec![silent]),
        ComposeError::NativeOnlyWithoutReason(_)
    ));
}

#[test]
fn sketch_composes_beside_sources() {
    let composition = Composition::new(vec![map(), sources(), sketch()]).expect("valid");
    assert_eq!(composition.descriptors().len(), 3);
}
