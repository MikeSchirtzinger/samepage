//! Native catalog, evidence acquisition and per-participant cursors. Portable
//! pane logic never opens a file, starts a process or creates another document.
use super::panes_catalog::Workspace;
use samepage_capability::{Capability, View};
use samepage_doc::journal::Journal;
use samepage_panes::{
    view::{self, Node},
    Panes,
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path, sync::Mutex};
use tokio::sync::Notify;

type Cursor = (Value, u64, BTreeMap<String, Value>);
fn meaningful(v: &View) -> BTreeMap<String, Value> {
    v.records()
        .filter(|r| samepage_panes::KINDS.contains(&r.kind.as_str()))
        .map(|r| {
            let props: BTreeMap<_, _> = r
                .props
                .iter()
                .filter(|(key, _)| {
                    !matches!(
                        key.as_str(),
                        "stack"
                            | "method"
                            | "created_by"
                            | "created_by_participant"
                            | "touched_by"
                            | "touched_by_participant"
                    )
                })
                .collect();
            (r.id.clone(), serde_json::to_value(props).unwrap())
        })
        .collect()
}
pub struct PanesHost {
    workspace: Workspace,
    cursors: Mutex<BTreeMap<String, Cursor>>,
    pub changed: Notify,
}
impl PanesHost {
    pub fn open(root: &Path) -> Result<Self, String> {
        Ok(Self {
            workspace: Workspace::open(root)?,
            cursors: Mutex::new(BTreeMap::new()),
            changed: Notify::new(),
        })
    }
    pub fn facts(&self, v: &View) -> Result<BTreeMap<String, Value>, String> {
        fn walk(node: &Node, workspace: &Workspace, facts: &mut BTreeMap<String, Value>) {
            match node {
                Node::Source { path, from, to } => {
                    facts.insert(
                        view::source_key(path, *from, *to),
                        workspace
                            .excerpt(path, *from, *to)
                            .unwrap_or_else(|error| json!({"error":error})),
                    );
                }
                Node::Options { filter } => {
                    facts.insert(
                        view::options_key(filter.as_deref()),
                        workspace
                            .catalog(filter.as_deref())
                            .unwrap_or_else(|error| json!({"error":error})),
                    );
                }
                Node::Stack { children, .. }
                | Node::Row { children, .. }
                | Node::Deck { children, .. } => {
                    for child in children {
                        walk(child, workspace, facts)
                    }
                }
                _ => {}
            }
        }
        let mut facts = BTreeMap::new();
        for r in v.records().filter(|r| r.kind == "pane") {
            let tree = r
                .props
                .get("tree")
                .and_then(|s| s.as_str())
                .ok_or("pane tree absent")?;
            let node: Node = serde_json::from_str(tree).map_err(|e| e.to_string())?;
            walk(&node, &self.workspace, &mut facts);
        }
        Ok(facts)
    }
    pub fn inspect(&self, v: &View, journal: &Journal) -> Result<Value, String> {
        let facts = self.facts(v)?;
        let mut projection = samepage_panes::projection(v, &facts).map_err(|e| e.to_string())?;
        let actors: BTreeMap<_, _> = journal
            .recent
            .iter()
            .map(|entry| {
                (
                    entry.actor.participant.as_str(),
                    entry.actor.caller.as_str(),
                )
            })
            .collect();
        for pane in projection["panes"]
            .as_array_mut()
            .ok_or("pane projection absent")?
        {
            if pane["author"] == "participant" {
                pane["author"] = json!(actors
                    .get(pane["by_id"].as_str().unwrap_or(""))
                    .copied()
                    .unwrap_or("participant"));
            }
        }
        Ok(
            json!({"projection":projection,"facts":facts,"workspace":{"root":self.workspace.root().display().to_string(),"runnables":self.workspace.option_count()}}),
        )
    }
    pub fn read(&self, v: &View, journal: &Journal, participant: &str) -> Result<String, String> {
        let present = self.inspect(v, journal)?;
        let mut text = samepage_panes::describe_projection(&present["projection"])
            .map_err(|e| e.to_string())?;
        if v.records().filter(|r| r.kind == "pane").any(|r| {
            r.props
                .get("tree")
                .and_then(|s| s.as_str())
                .and_then(|raw| serde_json::from_str::<Node>(raw).ok())
                .is_some_and(|n| view::shows_options(&n))
        }) {
            text.push_str("\nRUNNABLE CATALOG\n");
            text.push_str(&self.workspace.describe_options());
        }
        let digest = present["projection"].clone();
        let mut cursors = self.cursors.lock().map_err(|_| "pane cursor poisoned")?;
        if let Some((before, _, _)) = cursors.get(participant) {
            let changes = Panes
                .changes(before, &digest, participant)
                .map_err(|e| e.to_string())?;
            if !changes.is_empty() {
                text.push_str("\nCHANGED SINCE YOUR LAST READ\n");
                for change in changes {
                    text.push_str(&format!("- {change}\n"));
                }
            }
        }
        cursors.insert(participant.into(), (digest, journal.seq, meaningful(v)));
        Ok(text)
    }
    pub fn unread_from_others(
        &self,
        v: &View,
        journal: &Journal,
        participant: &str,
    ) -> Result<bool, String> {
        let cursors = self.cursors.lock().map_err(|_| "pane cursor poisoned")?;
        let Some((prior, seq, records)) = cursors.get(participant) else {
            return Ok(false);
        };
        let digest = self.inspect(v, journal)?["projection"].clone();
        let current = meaningful(v);
        let changed: std::collections::BTreeSet<_> = records
            .keys()
            .chain(current.keys())
            .filter(|id| records.get(*id) != current.get(*id))
            .collect();
        Ok(!samepage_panes::changes(prior, &digest).is_empty()
            && journal.recent.iter().any(|entry| {
                entry.seq > *seq
                    && entry.actor.participant != participant
                    && entry.visible_change
                    && entry
                        .created
                        .iter()
                        .chain(&entry.modified)
                        .chain(&entry.deleted)
                        .any(|id| changed.contains(id))
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use samepage_capability::Composition;
    use samepage_doc::{host::Host, Actor, Caller};
    use std::sync::Arc;
    fn host() -> Host {
        Host::in_memory(
            Composition::new(vec![
                samepage_panes::descriptor(),
                samepage_map::descriptor(),
            ])
            .unwrap(),
            vec![Arc::new(Panes), Arc::new(samepage_map::Map)],
        )
        .unwrap()
    }
    fn invoke(h: &Host, who: &str, name: &str, mut args: Value) {
        if name != "card_place" {
            args["expected_revision"] = json!(h.revision());
        }
        h.invoke(&Actor::new(who, Caller::Agent), name, args)
            .unwrap();
    }
    #[test]
    fn a_source_pane_that_stopped_resolving_says_so_instead_of_vanishing() {
        let root = std::env::temp_dir().join(format!("panes-native-source-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("evidence.rs");
        std::fs::write(&file, "fn actual_evidence() {}\n").unwrap();
        let panes = PanesHost::open(&root).unwrap();
        let h = host();
        invoke(
            &h,
            "a",
            "pane_put",
            json!({"id":"source","title":"Source","view":{"kind":"source","path":"evidence.rs","from":1,"to":1}}),
        );
        let present = panes.inspect(&h.view(), &h.journal()).unwrap();
        assert!(present.to_string().contains("actual_evidence"));
        std::fs::remove_file(file).unwrap();
        let text = panes.read(&h.view(), &h.journal(), "a").unwrap();
        assert!(text.contains("unavailable"), "{text}");
        assert!(text.contains("Source"));
        assert_eq!(h.view().records().filter(|r| r.kind == "pane").count(), 1);
        std::fs::remove_dir(root).unwrap();
    }
    #[test]
    fn per_caller_cursors_do_not_wake_for_own_changes_or_other_capabilities() {
        let panes = PanesHost::open(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
        let h = host();
        invoke(
            &h,
            "a",
            "pane_put",
            json!({"id":"topic","title":"Topic","view":{"kind":"text","text":"first"}}),
        );
        panes.read(&h.view(), &h.journal(), "a").unwrap();
        panes.read(&h.view(), &h.journal(), "b").unwrap();
        invoke(
            &h,
            "a",
            "pane_put",
            json!({"id":"topic","title":"Topic","view":{"kind":"text","text":"own rewrite"}}),
        );
        invoke(&h, "b", "card_place", json!({"label":"unrelated card"}));
        assert!(!panes
            .unread_from_others(&h.view(), &h.journal(), "a")
            .unwrap());
        assert!(panes
            .unread_from_others(&h.view(), &h.journal(), "b")
            .unwrap());
        assert!(panes
            .read(&h.view(), &h.journal(), "b")
            .unwrap()
            .contains("CHANGED"));
        assert!(!panes
            .unread_from_others(&h.view(), &h.journal(), "b")
            .unwrap());
        assert!(panes
            .read(&h.view(), &h.journal(), "a")
            .unwrap()
            .contains("CHANGED"));
    }
}
