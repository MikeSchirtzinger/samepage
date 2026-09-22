#![allow(dead_code)]

use std::cell::Cell;

use samepage_capability::{
    ActionDecl, Artifact, Audience, Composition, Descriptor, Effect, Record, View,
};

pub fn map() -> Descriptor {
    Descriptor::new("map", "0.1.0", Artifact::Wasm)
        .owns("node", &["label", "x", "y", "w"], "cards")
        .owns("edge", &["from", "to"], "links between cards")
        .action(
            ActionDecl::new(
                "card_place",
                Audience::Both,
                Effect::Mutate,
                "Place a card.",
            )
            .alias("atlas_place"),
        )
        .action(
            ActionDecl::new(
                "page_read",
                Audience::Agent,
                Effect::Query,
                "Read the page.",
            )
            .alias("atlas_read"),
        )
}

pub fn sources() -> Descriptor {
    Descriptor::new("sources", "0.1.0", Artifact::Wasm)
        .depends_on("map", "0.1.0", "binds cards to files")
        .extends("node", &["path", "lines"], "source binding on a card")
        .reads("edge")
        .action(
            ActionDecl::new(
                "source_read",
                Audience::Agent,
                Effect::Query,
                "Read a file.",
            )
            .alias("repo_read"),
        )
}

pub fn sketch() -> Descriptor {
    Descriptor::new("sketch", "0.1.0", Artifact::Wasm)
        .depends_on("map", "0.1.0", "lifts shapes into cards")
        .owns("shape", &["form", "x", "y", "on"], "freehand shapes")
        .reads("node")
}

pub fn composition() -> Composition {
    Composition::new(vec![map(), sources(), sketch()]).expect("valid composition")
}

/// A document with a card, a link, and records the composition does not
/// own: an extension key left by a disabled capability on a known card, and
/// a whole record of a kind nobody enabled owns.
pub fn view() -> View {
    View::new(
        "rev-1",
        vec![
            Record::new("n1", "node")
                .with("label", "API")
                .with("x", 10.0)
                .with("touched_by", "human")
                .with("state_variable", "cursor"),
            Record::new("n2", "node").with("label", "DB"),
            Record::new("e1", "edge")
                .with("from", "n1")
                .with("to", "n2"),
            Record::new("v1", "agent-ink-variable").with("value", 3.0),
        ],
    )
    .expect("valid view")
}

/// A minter that counts up from `id-0`.
pub fn counter() -> impl FnMut() -> String {
    let next = Cell::new(0);
    move || {
        let id = format!("id-{}", next.get());
        next.set(next.get() + 1);
        id
    }
}
