//! `GET /atlas/lanes`: what `samepage-extract` finds in the code, reconciled
//! against what the Map already draws.
//!
//! The rule this file exists to enforce: the agent's picture never gets to
//! be the picture. A lane the extractor found and no card claims renders
//! itself as `undeclared` regardless of what the diagram says, and nothing
//! on the Map can make it go away except claiming it (bind a real card to
//! it) or cementing its removal. See `src/tier.rs` for the tier this feeds.

use std::path::Path;

use samepage_extract::{Lane, Report};
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};

use crate::tier::{self, G8Status, TierRegistry};
use same_page_atlas_core::Atlas;

/// A stable id for a lane across scans, since the extractor does not mint
/// one. Built from exactly the evidence a reviewer would use to recognize
/// "the same lane": its file, its line, and its kind. Two different lanes on
/// the same line (rare, but the extractor's own dedup already prevents two
/// patterns reporting the same file:line) would collide; kind is folded in
/// so that never happens in practice.
pub fn lane_id(lane: &Lane) -> String {
    let mut hasher = Sha256::new();
    hasher.update(lane.evidence.path.to_string_lossy().as_bytes());
    hasher.update(b":");
    hasher.update(lane.evidence.line.to_string().as_bytes());
    hasher.update(b":");
    hasher.update(format!("{:?}", lane.kind).as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// The first line of a node's `lines` field (`"12"` or `"12-48"`), or `None`
/// for a node with no source binding at all.
fn node_start_line(lines: &str) -> Option<u32> {
    lines.split('-').next()?.trim().parse().ok()
}

/// Whether one of a card's bindings is close enough to a lane's evidence to
/// count as the same thing: same repository-relative file, and a line
/// within 5 either way. Exact-line equality would break the very first time
/// a human's edit shifted a bound line down by one; five lines of slack
/// survives that without also matching an unrelated function two hundred
/// lines away.
fn binding_matches(node_path: &str, node_lines: &str, lane: &Lane) -> bool {
    let node_path = node_path.trim().trim_start_matches("./");
    let lane_path = lane.evidence.path.to_string_lossy();
    let lane_path = lane_path.trim().trim_start_matches("./");
    if node_path != lane_path {
        return false;
    }
    match node_start_line(node_lines) {
        Some(start) => start.abs_diff(lane.evidence.line) <= 5,
        None => false,
    }
}

/// A card's full binding set (see `tier::effective_bindings`) checked
/// against one lane: true if ANY bound file is close enough.
fn any_binding_matches(node: &same_page_atlas_core::Node, registry: &TierRegistry, lane: &Lane) -> bool {
    tier::effective_bindings(node, registry)
        .iter()
        .any(|binding| binding_matches(&binding.path, &binding.lines, lane))
}

/// Run the extractor and reconcile its report against the current Atlas: for
/// each lane, either the id of the card that claims it, or nothing.
///
/// Reconciliation checks two independent things per lane, either one wins:
/// a live card with ANY bound file (its own `path`/`lines`, or anything
/// `atlas_claim_lane` added) falling within five lines of the lane's
/// evidence, or a card recorded in the registry as having explicitly
/// claimed this exact lane id, which survives every bound file drifting
/// further than that from the lane afterward.
///
/// A lane nothing claims may still not be plain `undeclared`: if
/// `atlas_remove_lane` cemented a removal obligation for it, it carries a
/// `removal` object instead, gate-unmet until G8 confirms the code is gone.
pub fn reconcile(report: &Report, atlas: &Atlas, registry: &TierRegistry, g8: &G8Status) -> JsonValue {
    let removal_json = |obligation_id: &str| {
        if g8.gate_met(obligation_id) {
            json!({"tier": "gate-met", "chip": "REMOVE met", "obligation_id": obligation_id})
        } else {
            json!({"tier": "gate-unmet", "chip": "REMOVE unmet", "obligation_id": obligation_id})
        }
    };
    let mut seen_lane_ids = std::collections::HashSet::new();
    let mut lanes: Vec<JsonValue> = report
        .lanes
        .iter()
        .map(|lane| {
            let id = lane_id(lane);
            seen_lane_ids.insert(id.clone());
            let matched = atlas
                .nodes
                .iter()
                .find(|node| any_binding_matches(node, registry, lane))
                .map(|node| node.id.clone())
                .or_else(|| {
                    registry
                        .lane_claims
                        .get(&id)
                        .filter(|node_id| atlas.node(node_id).is_some())
                        .cloned()
                });
            let removal = matched
                .is_none()
                .then(|| registry.removals.get(&id))
                .flatten()
                .map(|record| removal_json(&record.obligation_id));
            json!({
                "lane_id": id,
                "kind": format!("{:?}", lane.kind).to_lowercase(),
                "label": lane.label,
                "package": lane.package,
                "detail": lane.detail,
                "evidence": {
                    "path": lane.evidence.path,
                    "line": lane.evidence.line,
                    "snippet": lane.evidence.snippet,
                    "sha256": lane.evidence.sha256,
                },
                "matched_node_id": matched,
                "removal": removal,
            })
        })
        .collect();
    // A removal that SUCCEEDED means its lane no longer exists to scan: the
    // whole point of `atlas_remove_lane` is to make the code (and so the
    // lane) go away. Without this, the card would vanish the instant the
    // removal it asked for actually happened, rather than turning green.
    for (removed_lane_id, record) in &registry.removals {
        if seen_lane_ids.contains(removed_lane_id) {
            continue;
        }
        lanes.push(json!({
            "lane_id": removed_lane_id,
            "kind": record.kind,
            "label": record.label,
            "package": null,
            "detail": "no longer found in the code",
            "evidence": {
                "path": record.path,
                "line": record.line,
                "snippet": record.snippet,
                "sha256": null,
            },
            "matched_node_id": null,
            "removal": removal_json(&record.obligation_id),
        }));
    }
    let undeclared: Vec<JsonValue> = lanes
        .iter()
        .filter(|lane| lane["matched_node_id"].is_null())
        .cloned()
        .collect();
    json!({
        "ok": true,
        "root": report.root,
        "scanned_files": report.scanned_files,
        "languages": report.languages,
        "skipped": report.skipped.iter().map(|s| json!({"path": s.path, "reason": s.reason})).collect::<Vec<_>>(),
        "lanes": lanes,
        "undeclared": undeclared,
    })
}

/// Scan `root` with `samepage-extract`. No caching yet: static extraction
/// over a project this size runs well under the polling interval the
/// browser uses, and a wrong cached answer (a lane that stopped existing,
/// still reported) is a worse failure mode than a redundant scan. See the
/// brief's note on caching by file hashes; if this ever needs to skip
/// unchanged trees, the cache key belongs here, keyed on exactly the walk
/// `samepage_extract::scan` already performs.
pub fn scan(root: &Path) -> Result<Report, String> {
    samepage_extract::scan(root).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use samepage_extract::{Evidence, LaneKind};
    use std::path::PathBuf;

    fn lane(path: &str, line: u32) -> Lane {
        Lane {
            kind: LaneKind::Listener,
            label: "test".to_string(),
            evidence: Evidence {
                path: PathBuf::from(path),
                line,
                snippet: "TcpListener::bind(...)".to_string(),
                sha256: "deadbeef".to_string(),
            },
            package: None,
            detail: "a listener".to_string(),
        }
    }

    #[test]
    fn the_same_lane_gets_the_same_id_across_two_separate_scans() {
        assert_eq!(lane_id(&lane("src/main.rs", 12)), lane_id(&lane("src/main.rs", 12)));
    }

    #[test]
    fn a_different_line_or_kind_gets_a_different_id() {
        assert_ne!(lane_id(&lane("src/main.rs", 12)), lane_id(&lane("src/main.rs", 13)));
        let mut other_kind = lane("src/main.rs", 12);
        other_kind.kind = LaneKind::Spawn;
        assert_ne!(lane_id(&lane("src/main.rs", 12)), lane_id(&other_kind));
    }

    #[test]
    fn a_binding_within_five_lines_matches_and_six_does_not() {
        let close = lane("src/main.rs", 12);
        assert!(binding_matches("src/main.rs", "10", &close));
        assert!(binding_matches("src/main.rs", "17", &close));
        assert!(!binding_matches("src/main.rs", "18", &close));
        assert!(!binding_matches("other.rs", "12", &close));
    }

    fn empty_report() -> Report {
        Report {
            root: PathBuf::from("/repo"),
            lanes: vec![lane("src/main.rs", 4)],
            scanned_files: 1,
            skipped: vec![],
            languages: vec!["rust".to_string()],
        }
    }

    #[test]
    fn a_removal_whose_lane_is_still_in_the_scan_is_not_duplicated() {
        let report = empty_report();
        let atlas = Atlas::default();
        let live_lane_id = lane_id(&report.lanes[0]);
        let mut registry = TierRegistry::default();
        registry.removals.insert(
            live_lane_id.clone(),
            crate::tier::RemovalRecord {
                obligation_id: "ATLAS-REMOVE-aaaa1111".to_string(),
                kind: "listener".to_string(),
                label: "test".to_string(),
                path: "src/main.rs".to_string(),
                line: 4,
                snippet: "TcpListener::bind(...)".to_string(),
            },
        );
        let output = reconcile(&report, &atlas, &registry, &G8Status::NotCemented);
        let lanes = output["lanes"].as_array().unwrap();
        assert_eq!(lanes.len(), 1, "the still-live lane must not also appear as an orphaned removal");
        assert_eq!(lanes[0]["removal"]["chip"], "REMOVE unmet");
    }

    #[test]
    fn a_removal_whose_lane_vanished_from_the_scan_still_renders_and_can_read_gate_met() {
        let report = empty_report(); // does NOT contain the removed lane below
        let atlas = Atlas::default();
        let mut registry = TierRegistry::default();
        registry.removals.insert(
            "gone-lane-id".to_string(),
            crate::tier::RemovalRecord {
                obligation_id: "ATLAS-REMOVE-bbbb2222".to_string(),
                kind: "spawn".to_string(),
                label: "src/main.rs".to_string(),
                path: "src/main.rs".to_string(),
                line: 12,
                snippet: "Command::new(\"node\")".to_string(),
            },
        );
        let unmet = reconcile(&report, &atlas, &registry, &G8Status::NotCemented);
        let lanes = unmet["lanes"].as_array().unwrap();
        assert_eq!(lanes.len(), 2, "the live lane plus the orphaned removal");
        let orphan = lanes.iter().find(|l| l["lane_id"] == "gone-lane-id").expect("orphaned removal must still render");
        assert_eq!(orphan["removal"]["chip"], "REMOVE unmet");
        assert_eq!(orphan["matched_node_id"], JsonValue::Null);

        let clean: std::collections::HashSet<String> = ["ATLAS-REMOVE-bbbb2222".to_string()].into_iter().collect();
        let met = reconcile(&report, &atlas, &registry, &G8Status::Ran { clean });
        let lanes = met["lanes"].as_array().unwrap();
        let orphan = lanes.iter().find(|l| l["lane_id"] == "gone-lane-id").unwrap();
        assert_eq!(orphan["removal"]["chip"], "REMOVE met", "once G8 confirms the code is gone, the card must flip green");
    }
}
