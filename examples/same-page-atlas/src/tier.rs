//! Trust tiers: what a Map card is allowed to say about itself.
//!
//! A tier is never authored. An agent cannot set `verified` any more than it
//! can set its own performance review; the seven tiers below are each
//! computed fresh from something the host can check: a live `replaces` edge,
//! a card's bound files, a G8 check result, or the plain absence of any of
//! those. `undeclared` is the one tier with no home in a [`Node`] at all —
//! it exists only where the extractor found a lane and the map did not, and
//! it is recomputed on every reconciliation, never stored, which is what
//! makes it impossible to dismiss.
//!
//! A card is not one file. "API server" binds a port and spawns a sidecar,
//! and the whole point of this feature is that both stay visible under one
//! agreed card rather than one lane silently standing in for the other. So a
//! card's bindings are a SET: the node's own `path`/`lines` (however it was
//! first drawn) plus whatever `atlas_claim_lane` has added since. `verified`
//! means every bound file's hash is fresh; `drifted` means at least one
//! changed.
//!
//! The registry that *is* persisted here ([`TierRegistry`]) holds exactly the
//! things no CRDT node can honestly carry: which lanes a card has claimed,
//! the hash each bound file read as when it was last verified, which
//! obligation id a cemented card or a cemented removal produced. None of
//! that is a fact about the diagram; it is bookkeeping the host needs to
//! tell `verified` from `drifted` on the next read. Keeping it in a sidecar
//! JSON file next to the Atlas state (rather than as new `Node` fields)
//! means this whole feature adds not one byte to the shared CRDT schema
//! every replica already negotiates.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};

use crate::source::Repo;
use same_page_atlas_core::{Atlas, Node};

/// The seven trust tiers a card can render as. Never typed by an agent; see
/// module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Claimed,
    Verified,
    GateUnmet,
    GateMet,
    Drifted,
    Proposed,
    /// Never constructed here: an undeclared card is not a [`Node`] at all,
    /// it is a lane `src/lanes.rs` found with no claiming card. This variant
    /// exists so the tier vocabulary is complete in one place; the string it
    /// prints is what `lanes::reconcile` writes directly for an unclaimed
    /// lane's synthetic card.
    #[allow(dead_code)]
    Undeclared,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Claimed => "claimed",
            Tier::Verified => "verified",
            Tier::GateUnmet => "gate-unmet",
            Tier::GateMet => "gate-met",
            Tier::Drifted => "drifted",
            Tier::Proposed => "proposed",
            Tier::Undeclared => "undeclared",
        }
    }
}

/// One file:lines a card vouches for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Binding {
    pub path: String,
    pub lines: String,
}

/// The computed tier for one card, plus what the chip should say.
#[derive(Debug, Clone)]
pub struct NodeTier {
    pub tier: Tier,
    pub chip: String,
    pub sha256_prefix: Option<String>,
    /// True when some live card carries a `replaces` edge that targets this
    /// one; the card still renders its own tier, struck through besides.
    pub struck: bool,
    /// Every file this card currently vouches for (its own `path`/`lines`
    /// plus anything added by `atlas_claim_lane`), for the chip count and
    /// the source viewer's file picker.
    pub bindings: Vec<Binding>,
}

impl NodeTier {
    fn to_json(&self) -> JsonValue {
        json!({
            "tier": self.tier.as_str(),
            "chip": self.chip,
            "sha256_prefix": self.sha256_prefix,
            "struck": self.struck,
            "bindings": self.bindings,
        })
    }
}

/// Sidecar state no CRDT node can honestly hold. See module docs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierRegistry {
    #[serde(default = "default_mode")]
    pub mode: String,
    /// A composite key (see [`binding_key`]) -> the sha256 a bound file read
    /// as the last time this registry saw it resolve. The baseline is
    /// pinned lazily, the first time a binding is ever checked; `verified`
    /// and `drifted` are both judged against it afterward.
    #[serde(default)]
    pub binding_hashes: BTreeMap<String, String>,
    /// `node_id` -> bindings added via `atlas_claim_lane`, beyond the node's
    /// own `path`/`lines`. A card's *effective* binding set is always this
    /// plus its own path/lines; see [`effective_bindings`].
    #[serde(default)]
    pub extra_bindings: BTreeMap<String, Vec<Binding>>,
    /// `lane_id` -> the card that claimed it via `atlas_claim_lane`, so the
    /// claim survives a card's source drifting a few lines from the lane's
    /// own evidence line.
    #[serde(default)]
    pub lane_claims: BTreeMap<String, String>,
    /// `node_id` -> the obligation id `atlas_map_cement` minted for it, so a
    /// later review can ask G8 whether that exact obligation passed.
    #[serde(default)]
    pub cemented: BTreeMap<String, String>,
    /// `lane_id` -> what `atlas_remove_lane` cemented for it. An unclaimed
    /// lane with an entry here renders as a removal gate instead of a plain
    /// `undeclared` card; see `lanes::reconcile`. The evidence is saved here
    /// (not just the obligation id) because a *successful* removal makes
    /// the lane vanish from the next scan entirely — that is the whole
    /// point — so this is what lets the card go on rendering (now
    /// `gate-met`) instead of silently disappearing the moment it succeeds.
    #[serde(default)]
    pub removals: BTreeMap<String, RemovalRecord>,
}

/// What a removed lane looked like at the moment `atlas_remove_lane`
/// cemented it, kept so the card can still render after the code (and so
/// the lane itself) is gone from a fresh scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemovalRecord {
    pub obligation_id: String,
    pub kind: String,
    pub label: String,
    pub path: String,
    pub line: u32,
    pub snippet: String,
}

fn default_mode() -> String {
    "explain".to_string()
}

impl Default for TierRegistry {
    fn default() -> Self {
        Self {
            mode: default_mode(),
            binding_hashes: BTreeMap::new(),
            extra_bindings: BTreeMap::new(),
            lane_claims: BTreeMap::new(),
            cemented: BTreeMap::new(),
            removals: BTreeMap::new(),
        }
    }
}

pub const MODES: &[&str] = &["explain", "map", "cement"];

/// The key `binding_hashes` is keyed by: stable per (node, path, lines), and
/// never collides with another node's binding to the same file, since two
/// cards can each vouch for the same lane independently.
fn binding_key(node_id: &str, binding: &Binding) -> String {
    format!("{node_id}\u{1}{}\u{1}{}", binding.path, binding.lines)
}

impl TierRegistry {
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let temporary = path.with_extension("tmp");
        let encoded = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        std::fs::write(&temporary, encoded).map_err(|error| error.to_string())?;
        std::fs::rename(&temporary, path).map_err(|error| error.to_string())
    }
}

/// Where the sidecar registry lives: alongside the Atlas state file, so
/// `AGUI_ATLAS_STATE=/tmp/x/atlas.json` gets `/tmp/x/atlas-tiers.json`.
pub fn registry_path(state_path: &Path) -> PathBuf {
    let stem = state_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("atlas");
    state_path.with_file_name(format!("{stem}-tiers.json"))
}

/// A card's full binding set: its own `path`/`lines` (if any), plus whatever
/// `atlas_claim_lane` added, deduplicated. Order is stable (primary first,
/// then claimed bindings in the order they were added) so the chip and the
/// source viewer's file list do not reorder between reads.
pub fn effective_bindings(node: &Node, registry: &TierRegistry) -> Vec<Binding> {
    let mut bindings = Vec::new();
    if !node.path.trim().is_empty() {
        bindings.push(Binding {
            path: node.path.clone(),
            lines: node.lines.clone(),
        });
    }
    if let Some(extra) = registry.extra_bindings.get(&node.id) {
        for binding in extra {
            if !bindings.contains(binding) {
                bindings.push(binding.clone());
            }
        }
    }
    bindings
}

/// What the last `g8 check --enforce --json` run at a project root said, or
/// why it says nothing. Read fresh on every review; nothing here is cached,
/// because a cache could tell a card it was gate-met after the human fixed
/// the file that made it fail.
pub enum G8Status {
    /// No `specs/obligations-v0.1.json` at the project root: nothing has
    /// been cemented, so nothing here is gate-anything.
    NotCemented,
    /// The obligations file exists but the `g8` binary is not on `PATH`.
    NotInstalled,
    /// `g8 check` ran but its output could not be parsed as the JSON shape
    /// this reads. The message is surfaced through `Debug`/logging only;
    /// every gate still reads as unmet, which is the safe default.
    Unreadable(#[allow(dead_code)] String),
    /// Ran cleanly. An obligation id in `clean` passed *and* cleared the
    /// enforcement rigor floor; anything else cemented is gate-unmet.
    Ran { clean: std::collections::HashSet<String> },
}

impl G8Status {
    pub fn gate_met(&self, obligation_id: &str) -> bool {
        matches!(self, G8Status::Ran { clean } if clean.contains(obligation_id))
    }
}

pub fn read_g8_status(project_root: &Path) -> G8Status {
    if !project_root.join("specs/obligations-v0.1.json").is_file() {
        return G8Status::NotCemented;
    }
    let output = match std::process::Command::new("g8")
        .args(["check", "--enforce", "--json"])
        .current_dir(project_root)
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return G8Status::NotInstalled;
        }
        Err(error) => return G8Status::Unreadable(error.to_string()),
    };
    let parsed: JsonValue = match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(error) => return G8Status::Unreadable(format!("{error}: {output:?}")),
    };
    let failed: std::collections::HashSet<String> = parsed["enforcement_failures"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|failure| failure["obligation_id"].as_str())
        .map(str::to_string)
        .collect();
    let clean = parsed["obligations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|obligation| {
            let id = obligation["id"].as_str()?;
            let passed = obligation["status"].as_str() == Some("passed");
            (passed && !failed.contains(id)).then(|| id.to_string())
        })
        .collect();
    G8Status::Ran { clean }
}

/// Every live `replaces` edge, both directions: which card proposes a
/// replacement, and which card that targets (and should draw struck
/// through). A `replaces` edge is an ordinary authored edge with that exact
/// label; nothing in the CRDT schema changed to add this relation.
fn replaces_edges(atlas: &Atlas) -> (std::collections::HashSet<String>, std::collections::HashSet<String>) {
    let mut proposing = std::collections::HashSet::new();
    let mut struck = std::collections::HashSet::new();
    for edge in &atlas.edges {
        if edge.label.trim() == "replaces" {
            proposing.insert(edge.from.clone());
            struck.insert(edge.to.clone());
        }
    }
    (proposing, struck)
}

/// One binding's read: whether it resolved, and if so, whether its hash
/// matches its pinned baseline (pinning it fresh if this is the first time).
enum BindingRead {
    Unreadable,
    Fresh,
    Drifted,
}

fn read_binding(
    node_id: &str,
    binding: &Binding,
    repo: &Repo,
    registry: &mut TierRegistry,
    dirty: &mut bool,
) -> BindingRead {
    let Ok(excerpt) = repo.read(&binding.path, &binding.lines) else {
        return BindingRead::Unreadable;
    };
    let current = {
        let mut hasher = Sha256::new();
        hasher.update(excerpt.text.as_bytes());
        format!("{:x}", hasher.finalize())
    };
    let key = binding_key(node_id, binding);
    match registry.binding_hashes.get(&key) {
        None => {
            registry.binding_hashes.insert(key, current);
            *dirty = true;
            BindingRead::Fresh
        }
        Some(baseline) if *baseline == current => BindingRead::Fresh,
        Some(_) => BindingRead::Drifted,
    }
}

/// Compute every node's tier in one pass. Called on every `/atlas/review`,
/// so an edited file or a fixed obligation is reflected on the very next
/// read; nothing here survives a restart except what [`TierRegistry`] itself
/// persists (binding hashes, lane claims, cemented ids).
pub fn annotate(
    atlas: &Atlas,
    repo: &Repo,
    registry: &mut TierRegistry,
    registry_path: &Path,
    g8: &G8Status,
) -> BTreeMap<String, JsonValue> {
    let (proposing, struck_targets) = replaces_edges(atlas);
    let mut dirty = false;
    let mut out = BTreeMap::new();
    for node in &atlas.nodes {
        let struck = struck_targets.contains(&node.id);
        let bindings = effective_bindings(node, registry);
        let computed = if proposing.contains(&node.id) {
            NodeTier {
                tier: Tier::Proposed,
                chip: "PROPOSED".to_string(),
                sha256_prefix: None,
                struck,
                bindings,
            }
        } else if let Some(obligation_id) = registry.cemented.get(&node.id).cloned() {
            if g8.gate_met(&obligation_id) {
                NodeTier { tier: Tier::GateMet, chip: "GATE met".to_string(), sha256_prefix: None, struck, bindings }
            } else {
                NodeTier { tier: Tier::GateUnmet, chip: "GATE unmet".to_string(), sha256_prefix: None, struck, bindings }
            }
        } else if bindings.is_empty() {
            NodeTier { tier: Tier::Claimed, chip: "claimed".to_string(), sha256_prefix: None, struck, bindings }
        } else {
            let mut any_unreadable = false;
            let mut any_drifted = false;
            for binding in &bindings {
                match read_binding(&node.id, binding, repo, registry, &mut dirty) {
                    BindingRead::Unreadable => any_unreadable = true,
                    BindingRead::Drifted => any_drifted = true,
                    BindingRead::Fresh => {}
                }
            }
            if any_unreadable {
                NodeTier { tier: Tier::Claimed, chip: "claimed".to_string(), sha256_prefix: None, struck, bindings }
            } else if any_drifted {
                let chip = if bindings.len() > 1 {
                    format!("DRIFTED ({} lanes)", bindings.len())
                } else {
                    "DRIFTED".to_string()
                };
                NodeTier { tier: Tier::Drifted, chip, sha256_prefix: None, struck, bindings }
            } else if bindings.len() == 1 {
                let key = binding_key(&node.id, &bindings[0]);
                let hash = registry.binding_hashes.get(&key).cloned().unwrap_or_default();
                let prefix = hash.get(..6).unwrap_or(&hash).to_string();
                NodeTier {
                    tier: Tier::Verified,
                    chip: format!("verified {prefix}"),
                    sha256_prefix: Some(prefix),
                    struck,
                    bindings,
                }
            } else {
                NodeTier {
                    tier: Tier::Verified,
                    chip: format!("verified {} lanes", bindings.len()),
                    sha256_prefix: None,
                    struck,
                    bindings,
                }
            }
        };
        out.insert(node.id.clone(), computed.to_json());
    }
    if dirty {
        let _ = registry.save(registry_path);
    }
    out
}

/// Bind an existing card's source link to a lane's evidence, ADDING to its
/// binding set rather than replacing it, and pin the baseline hash at the
/// moment of binding so the very next drift check has something to compare
/// against. This is the only agent-writable path from `undeclared` to
/// `verified`: [`Tier::Undeclared`] never appears as a stored value
/// anywhere, it is what a lane with no entry here renders as.
pub fn record_claim(
    registry: &mut TierRegistry,
    registry_path: &Path,
    lane_id: &str,
    node_id: &str,
    path: &str,
    lines: &str,
    sha256: &str,
) -> Result<(), String> {
    let binding = Binding {
        path: path.to_string(),
        lines: lines.to_string(),
    };
    let entry = registry.extra_bindings.entry(node_id.to_string()).or_default();
    if !entry.contains(&binding) {
        entry.push(binding.clone());
    }
    registry
        .binding_hashes
        .insert(binding_key(node_id, &binding), sha256.to_string());
    registry.lane_claims.insert(lane_id.to_string(), node_id.to_string());
    registry.save(registry_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_registry_defaults_to_explain_mode_with_nothing_recorded() {
        let registry = TierRegistry::default();
        assert_eq!(registry.mode, "explain");
        assert!(registry.binding_hashes.is_empty());
        assert!(registry.extra_bindings.is_empty());
        assert!(registry.lane_claims.is_empty());
        assert!(registry.cemented.is_empty());
        assert!(registry.removals.is_empty());
    }

    #[test]
    fn registry_round_trips_through_its_sidecar_file() {
        let dir = std::env::temp_dir().join(format!("atlas-tier-registry-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("atlas-tiers.json");
        let mut registry = TierRegistry {
            mode: "map".to_string(),
            ..TierRegistry::default()
        };
        registry
            .binding_hashes
            .insert(binding_key("n1", &Binding { path: "a.rs".into(), lines: "1".into() }), "abc123".into());
        registry.save(&path).unwrap();
        let reloaded = TierRegistry::load(&path);
        assert_eq!(reloaded.mode, "map");
        assert_eq!(
            reloaded
                .binding_hashes
                .get(&binding_key("n1", &Binding { path: "a.rs".into(), lines: "1".into() }))
                .map(String::as_str),
            Some("abc123")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn registry_path_sits_beside_the_atlas_state_file() {
        let path = registry_path(Path::new("/tmp/x/atlas.json"));
        assert_eq!(path, Path::new("/tmp/x/atlas-tiers.json"));
    }

    #[test]
    fn a_project_with_no_obligations_file_reads_as_not_cemented() {
        let dir = std::env::temp_dir().join(format!("atlas-g8-none-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(matches!(read_g8_status(&dir), G8Status::NotCemented));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn node_with(id: &str, path: &str, lines: &str) -> Node {
        Node {
            id: id.to_string(),
            label: String::new(),
            path: path.to_string(),
            lines: lines.to_string(),
            note: String::new(),
            tone: String::new(),
            status: String::new(),
            cemented: String::new(),
            x: 0.0,
            y: 0.0,
            w: 240.0,
            h: 0.0,
            measured: false,
            created_by: "agent".to_string(),
            touched_by: "agent".to_string(),
            parent: String::new(),
            diagram_kind: String::new(),
            state_initial: false,
            state_terminal: false,
            state_variable: String::new(),
            state_previous_value: None,
            color: String::new(),
            emphasis: String::new(),
            size: String::new(),
            kind: String::new(),
        }
    }

    #[test]
    fn effective_bindings_is_the_primary_binding_plus_claimed_ones_deduplicated() {
        let node = node_with("n1", "src/main.rs", "4");
        let mut registry = TierRegistry::default();
        assert_eq!(
            effective_bindings(&node, &registry),
            vec![Binding { path: "src/main.rs".into(), lines: "4".into() }]
        );
        registry.extra_bindings.insert(
            "n1".to_string(),
            vec![
                Binding { path: "src/main.rs".into(), lines: "12".into() },
                Binding { path: "src/main.rs".into(), lines: "4".into() }, // duplicate of primary
            ],
        );
        assert_eq!(
            effective_bindings(&node, &registry),
            vec![
                Binding { path: "src/main.rs".into(), lines: "4".into() },
                Binding { path: "src/main.rs".into(), lines: "12".into() },
            ]
        );
    }

    #[test]
    fn a_node_with_no_path_and_no_claimed_bindings_has_none() {
        let node = node_with("n1", "", "");
        let registry = TierRegistry::default();
        assert!(effective_bindings(&node, &registry).is_empty());
    }
}
