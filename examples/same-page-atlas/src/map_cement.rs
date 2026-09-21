//! `atlas_map_cement` and `atlas_remove_lane`: turn the agreed Map into G8
//! obligations.
//!
//! Distinct from `cement.rs`, which turns a signed assertion-board draft
//! into a Govern advisory receipt for a *decision*. This module turns the
//! Map's own trust tiers, and a human's decision to remove a lane the
//! extractor found, into `specs/obligations-v0.1.json` at the project root
//! the atlas is about — the codebase, not the atlas's own state directory.
//! Every obligation this writes starts red on purpose: it has a real
//! `checker` (so it evaluates rather than sitting `Unknown`), but nothing
//! has ratified or attested it yet, so `g8 check --enforce` reports
//! `insufficient_rigor` until a human runs `g8 ratify` and `g8 attest`. That
//! red state is not a bug to route around; the brief calls for it plainly.
//!
//! Both operations UPSERT into the same file rather than each overwriting
//! it whole: cementing the Map again must not erase a removal a human
//! already cemented, and removing a lane must not erase the Map's own
//! obligations. See [`merge_and_write`].

use std::collections::BTreeMap;
use std::path::Path;

use samepage_extract::Lane;
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};

use crate::source::Repo;
use crate::tier::{Binding, TierRegistry};
use same_page_atlas_core::{Atlas, Node};

/// One regex-safe line to gate on: the excerpt's first non-blank trimmed
/// line, escaped for `rg`'s Rust-regex syntax. Gating on the whole excerpt
/// would make the pattern as fragile as the file itself; one anchor line is
/// enough to catch the file being edited out from under the claim without
/// also failing on unrelated changes elsewhere in the same range.
fn anchor_pattern(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    Some(regex_escape(line))
}

fn regex_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if "\\.+*?()|[]{}^$".contains(ch) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// A short, stable, G8-shaped id for one node: `ATLAS-CARD-<8 hex>`. Derived
/// from the node id rather than the label, so renaming a card never renames
/// its obligation out from under an existing ratification.
fn obligation_id(node_id: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(node_id.as_bytes()));
    format!("ATLAS-CARD-{}", &digest[..8])
}

/// A short, stable, G8-shaped id for one lane's removal:
/// `ATLAS-REMOVE-<8 hex>`, in a distinct namespace from card obligations so
/// the two kinds never collide and `atlas_map_cement` never touches a
/// removal it did not mint.
pub fn removal_obligation_id(lane_id: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(lane_id.as_bytes()));
    format!("ATLAS-REMOVE-{}", &digest[..8])
}

/// One check per binding, all in one obligation: `verified` (or `proposed`)
/// means every bound file's cited content still holds, so the gate that
/// cements that claim must check every one of them, not just the first.
/// `None` when any binding cannot be read or is empty text, since a
/// half-checkable gate is worse than an honest skip (see `CementResult::skipped`).
fn obligation_for(node: &Node, bindings: &[Binding], repo: &Repo, expected: JsonValue, note: &str) -> Option<JsonValue> {
    if bindings.is_empty() {
        return None;
    }
    let mut checks = Vec::with_capacity(bindings.len());
    for binding in bindings {
        let excerpt = repo.read(&binding.path, &binding.lines).ok()?;
        let pattern = anchor_pattern(&excerpt.text)?;
        checks.push(json!({
            "backend": "rg_match_count",
            "args": {
                "pattern": pattern,
                "glob": [binding.path],
                "expected": expected,
            }
        }));
    }
    Some(json!({
        "id": obligation_id(&node.id),
        "node_id": node.id,
        "label": node.label,
        "note": note,
        "checker": {
            "mode": "typed",
            "checks": checks,
        },
        "signal": { "gate": "atlas_map_conformance", "wiring": "repository_artifact", "advisory": false }
    }))
}

/// Build one removal obligation for an unclaimed lane: the lane's own
/// evidence snippet must no longer match, anywhere in its file. Does not
/// re-read the file: the evidence snippet the extractor already captured is
/// the pattern, so this can be built even after the line in question is
/// deleted (which is the point: proving the code is GONE, not present).
pub fn build_lane_removal(lane: &Lane, lane_id: &str, reason: &str) -> Option<JsonValue> {
    let path = lane.evidence.path.to_str()?;
    let pattern = regex_escape(lane.evidence.snippet.trim());
    if pattern.is_empty() {
        return None;
    }
    Some(json!({
        "id": removal_obligation_id(lane_id),
        "lane_id": lane_id,
        "label": lane.label,
        "note": reason,
        "checker": {
            "mode": "typed",
            "checks": [
                { "backend": "rg_match_count", "args": {
                    "pattern": pattern,
                    "glob": [path],
                    "expected": {"kind": "exactly", "value": 0},
                } }
            ]
        },
        "signal": { "gate": "atlas_map_conformance", "wiring": "repository_artifact", "advisory": false }
    }))
}

/// What `atlas_map_cement` wrote, or chose not to and why.
pub struct CementResult {
    pub path: std::path::PathBuf,
    /// New or updated obligations this call is minting, keyed by id inside
    /// each object. Only these are upserted; see [`merge_and_write`].
    pub obligations: Vec<JsonValue>,
    /// `node_id` -> minted obligation id, for the registry to remember.
    pub minted: Vec<(String, String)>,
    /// Cards that qualified for a gate but had nothing checkable to write
    /// (no bound source), named honestly instead of a fabricated gate.
    pub skipped: Vec<String>,
}

/// Build (but do not write) the new obligations for the current Map: one
/// obligation per verified or proposed card (one check per bound lane), one
/// removal obligation per card struck by a live `replaces` edge.
pub fn build(
    atlas: &Atlas,
    repo: &Repo,
    registry: &TierRegistry,
    tiers: &BTreeMap<String, JsonValue>,
) -> CementResult {
    let mut obligations = Vec::new();
    let mut minted = Vec::new();
    let mut skipped = Vec::new();
    for node in &atlas.nodes {
        let Some(computed) = tiers.get(&node.id) else { continue };
        let tier = computed["tier"].as_str().unwrap_or("");
        let struck = computed["struck"].as_bool().unwrap_or(false);
        let bindings = crate::tier::effective_bindings(node, registry);
        if struck {
            match obligation_for(
                node,
                &bindings,
                repo,
                json!({"kind":"exactly","value":0}),
                "removal gate: the replaced card's own bound content must be gone",
            ) {
                Some(obligation) => {
                    minted.push((node.id.clone(), obligation["id"].as_str().unwrap().to_string()));
                    obligations.push(obligation);
                }
                None => skipped.push(format!("{} (struck, no checkable source)", node.label)),
            }
            continue;
        }
        if tier == "verified" || tier == "proposed" {
            match obligation_for(
                node,
                &bindings,
                repo,
                json!({"kind":"at_least","value":1}),
                "the card's cited content must still be in every file it vouches for",
            ) {
                Some(obligation) => {
                    minted.push((node.id.clone(), obligation["id"].as_str().unwrap().to_string()));
                    obligations.push(obligation);
                }
                None => skipped.push(format!("{} ({tier}, no checkable source)", node.label)),
            }
        }
    }
    CementResult {
        path: repo.root().join("specs/obligations-v0.1.json"),
        obligations,
        minted,
        skipped,
    }
}

/// Read the obligations file at `path` (if any), upsert `entries` into it by
/// id, and write the merged whole back. An entry this call did not mention
/// (a removal `atlas_remove_lane` cemented, or a card obligation from an
/// earlier cement) survives untouched; only ids present in `entries` are
/// added or replaced.
pub fn merge_and_write(path: &Path, entries: Vec<JsonValue>) -> Result<(), String> {
    let mut by_id: BTreeMap<String, JsonValue> = match std::fs::read(path) {
        Ok(bytes) => {
            let existing: JsonValue = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            existing["obligations"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|obligation| Some((obligation["id"].as_str()?.to_string(), obligation.clone())))
                .collect()
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(error) => return Err(error.to_string()),
    };
    for entry in entries {
        if let Some(id) = entry["id"].as_str() {
            by_id.insert(id.to_string(), entry);
        }
    }
    let document = json!({
        "meta": {
            "project": "same-page-atlas Map cement",
            "note": "Every obligation here starts unratified and unattested. Run `g8 ratify` then `g8 attest <id> --files <path>` per obligation to clear it; until then `g8 check --enforce` reports insufficient_rigor.",
        },
        "obligations": by_id.into_values().collect::<Vec<_>>(),
    });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let encoded = serde_json::to_vec_pretty(&document).map_err(|error| error.to_string())?;
    std::fs::write(path, encoded).map_err(|error| error.to_string())
}

/// Write the built document (merged with whatever is already there) and
/// record which obligation belongs to which node in the sidecar registry,
/// so the next review can ask G8 whether it passed.
pub fn write(result: &CementResult, registry: &mut TierRegistry, registry_path: &Path) -> Result<(), String> {
    merge_and_write(&result.path, result.obligations.clone())?;
    for (node_id, obligation_id) in &result.minted {
        registry.cemented.insert(node_id.clone(), obligation_id.clone());
    }
    registry.save(registry_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_node_id_always_mints_the_same_obligation_id() {
        assert_eq!(obligation_id("n1"), obligation_id("n1"));
        assert_ne!(obligation_id("n1"), obligation_id("n2"));
        assert!(obligation_id("n1").starts_with("ATLAS-CARD-"));
    }

    #[test]
    fn removal_ids_live_in_a_distinct_namespace_from_card_ids() {
        assert!(removal_obligation_id("lane1").starts_with("ATLAS-REMOVE-"));
        assert_ne!(removal_obligation_id("n1"), obligation_id("n1"));
    }

    #[test]
    fn the_anchor_pattern_escapes_regex_metacharacters_and_skips_blank_lines() {
        assert_eq!(anchor_pattern("\n  \nfn main() {\n"), Some("fn main\\(\\) \\{".to_string()));
        assert_eq!(anchor_pattern("   \n  \n"), None);
    }

    #[test]
    fn merge_and_write_upserts_by_id_and_preserves_untouched_entries() {
        let dir = std::env::temp_dir().join(format!("atlas-cement-merge-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("obligations-v0.1.json");
        merge_and_write(&path, vec![json!({"id": "A", "value": 1})]).unwrap();
        merge_and_write(&path, vec![json!({"id": "B", "value": 2})]).unwrap();
        let document: JsonValue = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let ids: Vec<&str> = document["obligations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["A", "B"], "the second write must not erase the first");
        merge_and_write(&path, vec![json!({"id": "A", "value": 99})]).unwrap();
        let document: JsonValue = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let a = document["obligations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["id"] == "A")
            .unwrap();
        assert_eq!(a["value"], 99, "an id present in the new entries must be replaced, not duplicated");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
