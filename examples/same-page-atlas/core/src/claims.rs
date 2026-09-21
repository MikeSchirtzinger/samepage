//! Claims: what the agent says it understands about a node, with a stated
//! basis, and what the human said back.
//!
//! The atlas is one-to-one with what the agent believes. A review is the part
//! where a human picks one node and asks until both hold the same
//! understanding. That discussion used to live in the transcript and scroll
//! away. Here it lives on the node: each claim carries how the agent knows it
//! (`verified`, `inferred`, `assumed`, `unknown`) and the human's verdict
//! (`open`, `accepted`, `rejected`). A node with an open or rejected claim is
//! UNDER CHALLENGE; a node whose live claims are all accepted is SETTLED. Both
//! words are derived at read time and never stored. Contract:
//! `docs/design-challenge-v0.md`.

use ag_ui_canvas::ids::ObjectId;
use ag_ui_canvas::scene::{Author, PropValue, Scene};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    Atlas, AtlasError, BASES, Digest, KIND_CLAIM, MAX_CLAIMS_PER_NODE, MAX_TEXT, Problem, VERDICTS,
    capacity, line_range, one_of, read, revision_hash, source_path, text,
};

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Deserialize)]
pub struct Claim {
    pub id: String,
    /// The node this claim is about.
    pub about: String,
    pub text: String,
    /// One of [`BASES`].
    pub basis: String,
    pub path: String,
    pub lines: String,
    /// The commit the source was read at: forty hex characters, or empty when
    /// the claim is pinned to no revision. A `verified` claim may carry one so
    /// a later reader can check the same range at the same commit rather than
    /// at whatever the working tree happens to say today.
    pub revision: String,
    /// One of [`VERDICTS`]. Only the human writes it.
    pub verdict: String,
    /// Set once by the agent, never cleared. A withdrawn claim keeps its
    /// history and stops counting.
    pub withdrawn: bool,
    pub created_by: String,
    pub touched_by: String,
}

impl Claim {
    pub fn live(&self) -> bool {
        !self.withdrawn
    }

    /// `path:lines@revision` when both a range and a commit are pinned,
    /// `path:lines` or a bare path otherwise. Empty when there is no source.
    pub fn source_ref(&self) -> String {
        if self.path.is_empty() {
            return String::new();
        }
        let mut out = if self.lines.is_empty() {
            self.path.clone()
        } else {
            format!("{}:{}", self.path, self.lines)
        };
        if !self.revision.is_empty() {
            out.push('@');
            out.push_str(&self.revision);
        }
        out
    }
}

/// A claim in a form two turns apart can be compared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimDigest {
    pub id: String,
    pub about: String,
    pub text: String,
    pub basis: String,
    /// `path:lines@revision`, exactly as [`Claim::source_ref`] renders it.
    pub source: String,
    /// The pinned commit on its own, so a reader that wants the revision does
    /// not have to take `source` apart to find it.
    #[serde(default)]
    pub revision: String,
    pub verdict: String,
    pub withdrawn: bool,
    pub touched_by: String,
}

/// Projection healing: a claim about a node that is not on the atlas is
/// dropped, the same way a dangling `parent` is healed to the top level.
pub(crate) fn heal(atlas: &mut Atlas) {
    let nodes: std::collections::HashSet<&str> =
        atlas.nodes.iter().map(|node| node.id.as_str()).collect();
    atlas
        .claims
        .retain(|claim| nodes.contains(claim.about.as_str()));
    atlas.claims.sort_by(|a, b| a.id.cmp(&b.id));
}

fn agent_only(author: &Author, what: &str) -> Result<(), AtlasError> {
    if matches!(author, Author::Human) {
        return Err(format!("only the agent can {what}"));
    }
    Ok(())
}

/// What a claim says about where it came from, once validated.
struct Cited {
    basis: String,
    path: String,
    lines: String,
    revision: String,
}

/// Validate a basis against its source per the contract: `verified` needs
/// both `path` and `lines`; `assumed` and `unknown` refuse a source. A
/// revision may only be pinned by a `verified` claim, because the pin is a
/// statement that this exact range was read at that exact commit, and the
/// other three bases are the ones that did not read it.
fn basis_and_source(
    basis: &str,
    path: &str,
    lines: &str,
    revision: &str,
) -> Result<Cited, AtlasError> {
    let basis = one_of(basis, BASES, "basis")?;
    let path = source_path(path)?;
    let lines = line_range(lines)?;
    let revision = revision_hash(revision)?;
    match basis.as_str() {
        "verified" if path.is_empty() || lines.is_empty() => Err(
            "a verified claim needs both path and lines; use inferred if you cannot point at the range"
                .to_string(),
        ),
        "assumed" | "unknown" if !path.is_empty() || !lines.is_empty() => Err(format!(
            "an {basis} claim cannot carry a source; if you can cite it, it is verified or inferred"
        )),
        other if !revision.is_empty() && other != "verified" => Err(format!(
            "an {other} claim cannot pin a revision; a pinned commit says the range was read there, which is what verified means"
        )),
        _ => Ok(Cited {
            basis,
            path,
            lines,
            revision,
        }),
    }
}

/// The agent states one claim about a node, pinned to no revision.
pub fn claim(
    scene: &mut Scene,
    about: &str,
    text_body: &str,
    basis: &str,
    path: &str,
    lines: &str,
    author: &Author,
) -> Result<String, AtlasError> {
    claim_at(scene, about, text_body, basis, path, lines, "", author)
}

/// The agent states one claim about a node, optionally pinning the commit its
/// source was read at. Returns the claim id.
#[allow(clippy::too_many_arguments)]
pub fn claim_at(
    scene: &mut Scene,
    about: &str,
    text_body: &str,
    basis: &str,
    path: &str,
    lines: &str,
    revision: &str,
    author: &Author,
) -> Result<String, AtlasError> {
    agent_only(author, "create a claim")?;
    let atlas = read(scene)?;
    capacity(&atlas)?;
    let node = atlas
        .node(about)
        .ok_or_else(|| format!("no node {about:?} on the atlas; claims are about nodes only"))?;
    let live = atlas
        .claims_of(about)
        .iter()
        .filter(|claim| claim.live())
        .count();
    if live >= MAX_CLAIMS_PER_NODE {
        return Err(format!(
            "\"{}\" already carries {MAX_CLAIMS_PER_NODE} live claims; a node that needs more is two nodes, split it first",
            node.label
        ));
    }
    let cited = basis_and_source(basis, path, lines, revision)?;
    let text_body = text(text_body, MAX_TEXT, "claim text")?;
    if text_body.is_empty() {
        return Err("a claim needs text".to_string());
    }
    let id = scene
        .create_object_with_props(
            KIND_CLAIM,
            author.clone(),
            &[
                ("about", PropValue::Str(about.to_string())),
                ("text", PropValue::Str(text_body)),
                ("basis", PropValue::Str(cited.basis)),
                ("path", PropValue::Str(cited.path)),
                ("lines", PropValue::Str(cited.lines)),
                ("revision", PropValue::Str(cited.revision)),
                ("verdict", PropValue::Str("open".to_string())),
                ("withdrawn", PropValue::Bool(false)),
                ("touched_by", PropValue::Str(author.as_str().to_string())),
            ],
        )
        .map_err(|error| format!("could not create claim: {error}"))?;
    Ok(id.into_string())
}

/// The agent restates a claim, pinned to no revision.
pub fn revise_claim(
    scene: &Scene,
    id: &str,
    text_body: &str,
    basis: &str,
    path: &str,
    lines: &str,
    author: &Author,
) -> Result<(), AtlasError> {
    revise_claim_at(scene, id, text_body, basis, path, lines, "", author)
}

/// The agent restates a claim, optionally pinning the commit its source was
/// read at. An accepted claim goes back to `open`, because the human accepted
/// different words.
#[allow(clippy::too_many_arguments)]
pub fn revise_claim_at(
    scene: &Scene,
    id: &str,
    text_body: &str,
    basis: &str,
    path: &str,
    lines: &str,
    revision: &str,
    author: &Author,
) -> Result<(), AtlasError> {
    agent_only(author, "revise a claim")?;
    let atlas = read(scene)?;
    let existing = atlas
        .claim(id)
        .ok_or_else(|| format!("no claim {id:?} on the atlas"))?;
    if existing.withdrawn {
        return Err(format!("claim {id} was withdrawn; state a new one instead"));
    }
    let cited = basis_and_source(basis, path, lines, revision)?;
    let text_body = text(text_body, MAX_TEXT, "claim text")?;
    if text_body.is_empty() {
        return Err("a claim needs text".to_string());
    }
    let mut props = vec![
        ("text", PropValue::Str(text_body)),
        ("basis", PropValue::Str(cited.basis)),
        ("path", PropValue::Str(cited.path)),
        ("lines", PropValue::Str(cited.lines)),
        ("revision", PropValue::Str(cited.revision)),
        ("touched_by", PropValue::Str(author.as_str().to_string())),
    ];
    if existing.verdict == "accepted" {
        props.push(("verdict", PropValue::Str("open".to_string())));
    }
    scene
        .set_props(&ObjectId::from(id.to_string()), &props)
        .map_err(|error| format!("could not revise claim: {error}"))
}

/// The agent stops standing behind a claim. Kept, not deleted.
pub fn withdraw_claim(scene: &Scene, id: &str, author: &Author) -> Result<(), AtlasError> {
    agent_only(author, "withdraw a claim")?;
    let atlas = read(scene)?;
    if atlas.claim(id).is_none() {
        return Err(format!("no claim {id:?} on the atlas"));
    }
    scene
        .set_props(
            &ObjectId::from(id.to_string()),
            &[
                ("withdrawn", PropValue::Bool(true)),
                ("touched_by", PropValue::Str(author.as_str().to_string())),
            ],
        )
        .map_err(|error| format!("could not withdraw claim: {error}"))
}

/// The human accepts or rejects a claim, or reopens it.
pub fn claim_verdict(
    scene: &Scene,
    id: &str,
    verdict: &str,
    author: &Author,
) -> Result<(), AtlasError> {
    if !matches!(author, Author::Human) {
        return Err("only the human can give a verdict on a claim".to_string());
    }
    let atlas = read(scene)?;
    let existing = atlas
        .claim(id)
        .ok_or_else(|| format!("no claim {id:?} on the atlas"))?;
    if existing.withdrawn {
        return Err(format!(
            "claim {id} was withdrawn by the agent; there is nothing to accept or reject"
        ));
    }
    let verdict = one_of(verdict, VERDICTS, "verdict")?;
    if verdict == "accepted" && existing.basis == "unknown" {
        return Err(format!(
            "claim {id} has unknown basis and cannot be accepted; revise it with what is known or withdraw it"
        ));
    }
    scene
        .set_props(
            &ObjectId::from(id.to_string()),
            &[
                ("verdict", PropValue::Str(verdict)),
                ("touched_by", PropValue::Str(author.as_str().to_string())),
            ],
        )
        .map_err(|error| format!("could not record verdict: {error}"))
}

/// Record the durable decision that covers a settled node selection.
///
/// The host publishes the ADR bundle first and calls this while it still owns
/// the human action. Every precondition is checked again against the live CRDT
/// before the first node changes, so a concurrent challenge cannot be stamped
/// as cemented from an older projection.
pub fn record_cemented(
    scene: &Scene,
    ids: &[String],
    adr_path: &str,
    document_revision: &str,
) -> Result<(), AtlasError> {
    let content = cement_content(&read(scene)?, ids)?;
    record_cemented_checked(scene, ids, adr_path, document_revision, &content)
}

/// Record only the selection content used to publish the ADR. The host holds
/// its scene lock across this check and the writes.
pub fn record_cemented_checked(
    scene: &Scene,
    ids: &[String],
    adr_path: &str,
    document_revision: &str,
    expected_content: &str,
) -> Result<(), AtlasError> {
    record_cemented_gate_checked(
        scene,
        ids,
        adr_path,
        document_revision,
        expected_content,
        "",
    )
}

pub fn record_cemented_gate_checked(
    scene: &Scene,
    ids: &[String],
    adr_path: &str,
    document_revision: &str,
    expected_content: &str,
    g8_status: &str,
) -> Result<(), AtlasError> {
    if !matches!(g8_status, "" | "active_passing" | "active_blocked") {
        return Err("invalid G8 adoption status".into());
    }
    if ids.is_empty() {
        return Err("cannot record cement for an empty node selection".to_string());
    }
    if adr_path.trim().is_empty() || document_revision.trim().is_empty() {
        return Err("cement needs both an ADR path and document revision".to_string());
    }
    let atlas = read(scene)?;
    let content = cement_content(&atlas, ids)?;
    if content != expected_content {
        return Err(
            "the selected decision changed while cement was publishing; review it again".into(),
        );
    }
    for id in ids {
        let node = atlas
            .node(id)
            .ok_or_else(|| format!("no node {id:?} on the atlas"))?;
        if atlas.under_challenge(id) {
            let blocking = atlas
                .claims_of(id)
                .into_iter()
                .find(|claim| claim.live() && claim.verdict != "accepted")
                .ok_or_else(|| format!("node {:?} has inconsistent challenge state", node.label))?;
            return Err(format!(
                "node {:?} is under challenge because claim {} is {}",
                node.label, blocking.id, blocking.verdict
            ));
        }
        if node.status != "agreed" || !atlas.settled(id) {
            return Err(format!(
                "node {:?} is not both agreed and settled",
                node.label
            ));
        }
    }
    let coverage = serde_json::to_string(&CementCoverage {
        ids: ids.to_vec(),
        content,
        g8_status: g8_status.into(),
    })
    .map_err(|error| format!("could not encode cement coverage: {error}"))?;
    for id in ids {
        let cemented = format!("{adr_path}\n{document_revision}\n{coverage}");
        scene
            .set_props(
                &ObjectId::from(id.clone()),
                &[
                    ("cemented", PropValue::Str(cemented)),
                    (
                        "touched_by",
                        PropValue::Str(Author::Human.as_str().to_string()),
                    ),
                ],
            )
            .map_err(|error| format!("could not record cement on node {id}: {error}"))?;
    }
    Ok(())
}

/// `place_node` calls this before letting a node become `agreed`.
pub(crate) fn refuse_agreed_under_challenge(atlas: &Atlas, id: &str) -> Result<(), AtlasError> {
    if !atlas.under_challenge(id) {
        return Ok(());
    }
    let open: Vec<String> = atlas
        .claims_of(id)
        .iter()
        .filter(|claim| claim.live() && claim.verdict != "accepted")
        .map(|claim| format!("{} ({})", claim.id, claim.verdict))
        .collect();
    Err(format!(
        "\"{}\" is under challenge and cannot be agreed until the human accepts every claim; still open: {}",
        atlas.node(id).map(|node| node.label.as_str()).unwrap_or(id),
        open.join(", ")
    ))
}

struct Tally {
    live: usize,
    open: usize,
    rejected: usize,
    accepted: usize,
}

fn tally(claims: &[&Claim]) -> Tally {
    let live: Vec<&&Claim> = claims.iter().filter(|claim| claim.live()).collect();
    Tally {
        live: live.len(),
        open: live.iter().filter(|c| c.verdict == "open").count(),
        rejected: live.iter().filter(|c| c.verdict == "rejected").count(),
        accepted: live.iter().filter(|c| c.verdict == "accepted").count(),
    }
}

fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        format!("{count} {word}")
    } else {
        format!("{count} {word}s")
    }
}

/// The summary that follows a node's label in the CLAIMS section.
fn summary_for(atlas: &Atlas, node_id: &str, claims: &[&Claim]) -> Option<String> {
    let t = tally(claims);
    if t.live == 0 {
        return None;
    }
    let label = atlas
        .node(node_id)
        .map(|node| node.label.clone())
        .unwrap_or_else(|| node_id.to_string());
    Some(if t.open + t.rejected > 0 {
        let state = if t.rejected > 0 {
            "DISAGREED"
        } else if atlas
            .marks
            .iter()
            .any(|mark| mark.target == node_id && matches!(mark.glyph.as_str(), "?" | "!"))
        {
            "QUESTIONED"
        } else {
            "UNREVIEWED"
        };
        format!(
            "\"{label}\" {state}: {}, {} open, {} rejected, {} accepted",
            plural(t.live, "claim"),
            t.open,
            t.rejected,
            t.accepted
        )
    } else if t.live == 1 {
        format!("\"{label}\" SETTLED: 1 claim, accepted")
    } else {
        format!(
            "\"{label}\" SETTLED: {}, all accepted",
            plural(t.live, "claim")
        )
    })
}

/// The summary that follows a node's label in the CLAIMS section.
pub fn summary_line(atlas: &Atlas, node_id: &str) -> Option<String> {
    summary_for(atlas, node_id, &atlas.claims_of(node_id))
}

/// One node's standing, in the exact words both surfaces show.
///
/// The browser used to derive all of this from the claim list itself, which
/// meant two implementations of one rule and no way to notice when they drift
/// apart. It reads this instead: the core is the only place that decides what
/// UNDER CHALLENGE means and what the badge says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NodeChallenge {
    /// Human-facing understanding state, distinct from the admission predicate.
    pub review_state: String,
    /// `open`, `settled`, `cemented`, or empty with no live claim.
    pub standing: String,
    /// Live claims the human has not accepted. Zero when settled.
    pub unsettled: usize,
    /// The badge: `3 to settle`, `settled`, `cemented`, or empty.
    pub badge: String,
    /// The CLAIMS read-back line for this node, or empty. The badge's title,
    /// so hovering a badge says exactly what a read-back would say.
    pub summary: String,
}

fn challenge_for(atlas: &Atlas, node_id: &str, claims: &[&Claim]) -> NodeChallenge {
    let t = tally(claims);
    if t.live == 0 {
        return NodeChallenge::default();
    }
    let unsettled = t.open + t.rejected;
    let cemented = atlas
        .node(node_id)
        .is_some_and(|node| cement_covers(atlas, node));
    let questioned = atlas
        .marks
        .iter()
        .any(|mark| mark.target == node_id && matches!(mark.glyph.as_str(), "?" | "!"));
    let review_state = if t.rejected > 0 {
        "disagreed"
    } else if unsettled > 0 && questioned {
        "questioned"
    } else if unsettled > 0 {
        "unreviewed"
    } else if cemented {
        "cemented"
    } else {
        "settled"
    };
    let (standing, badge) = if unsettled > 0 {
        (
            "open",
            if t.rejected > 0 {
                format!("{} disagreed", t.rejected)
            } else if questioned {
                "questioned".into()
            } else {
                format!("{unsettled} to review")
            },
        )
    } else if cemented {
        ("cemented", "cemented".to_string())
    } else {
        ("settled", "settled".to_string())
    };
    NodeChallenge {
        review_state: review_state.into(),
        standing: standing.to_string(),
        unsettled,
        badge,
        summary: summary_for(atlas, node_id, claims).unwrap_or_default(),
    }
}

/// What the badge on one node says. Empty everywhere for a node with no live
/// claim, which is how the browser knows to show no badge at all.
pub fn challenge(atlas: &Atlas, node_id: &str) -> NodeChallenge {
    challenge_for(atlas, node_id, &atlas.claims_of(node_id))
}

/// What a collapsed container's badge says about its whole subtree.
///
/// The container itself is included. Descendants are selected from the healed
/// containment tree, so a dangling or cyclic parent cannot double count a
/// claim. The returned shape and wording are exactly the per-node challenge
/// contract; only the set of claims being tallied is wider.
pub fn challenge_within(atlas: &Atlas, node_id: &str) -> NodeChallenge {
    if atlas.node(node_id).is_none() {
        return NodeChallenge::default();
    }
    let claims: Vec<&Claim> = atlas
        .claims
        .iter()
        .filter(|claim| {
            claim.about == node_id || atlas.contains_node(node_id, claim.about.as_str())
        })
        .collect();
    challenge_for(atlas, node_id, &claims)
}

impl Atlas {
    /// See [`challenge`].
    pub fn challenge(&self, node_id: &str) -> NodeChallenge {
        challenge(self, node_id)
    }

    /// See [`challenge_within`].
    pub fn challenge_within(&self, node_id: &str) -> NodeChallenge {
        challenge_within(self, node_id)
    }
}

#[derive(Serialize, Deserialize)]
struct CementCoverage {
    ids: Vec<String>,
    content: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    g8_status: String,
}

/// Content covered by a decision, independent of camera, measured geometry,
/// marks, and unrelated nodes. Both replicas compute this same identity.
pub fn cement_content(atlas: &Atlas, ids: &[String]) -> Result<String, AtlasError> {
    let selected: std::collections::BTreeSet<_> = ids.iter().map(String::as_str).collect();
    if selected.is_empty() || selected.len() != ids.len() {
        return Err("cement needs a nonempty selection of distinct nodes".into());
    }
    let mut nodes = Vec::new();
    for id in &selected {
        let node = atlas
            .node(id)
            .ok_or_else(|| format!("no node {id:?} on the atlas"))?;
        let claims: Vec<_> = atlas
            .claims_of(id)
            .into_iter()
            .filter(|claim| claim.live())
            .map(|claim| {
                (
                    &claim.id,
                    &claim.text,
                    &claim.basis,
                    &claim.path,
                    &claim.lines,
                    &claim.revision,
                    &claim.verdict,
                )
            })
            .collect();
        nodes.push(serde_json::json!({"id": node.id, "label": node.label,
            "path": node.path, "lines": node.lines, "note": node.note,
            "tone": node.tone, "status": node.status, "claims": claims}));
    }
    let edges: Vec<_> = atlas
        .edges
        .iter()
        .filter(|edge| selected.contains(edge.from.as_str()) && selected.contains(edge.to.as_str()))
        .map(|edge| {
            (
                &edge.id,
                &edge.from,
                &edge.to,
                &edge.label,
                &edge.event,
                &edge.guard,
            )
        })
        .collect();
    let bytes = serde_json::to_vec(&(nodes, edges))
        .map_err(|error| format!("could not encode decision content: {error}"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn cement_record(node: &crate::Node) -> Option<(&str, &str, Option<&str>)> {
    let mut parts = node.cemented.splitn(3, '\n');
    let path = parts.next()?;
    let revision = parts.next()?;
    if path.is_empty() || revision.is_empty() {
        return None;
    }
    Some((path, revision, parts.next()))
}

fn cement_covers(atlas: &Atlas, node: &crate::Node) -> bool {
    let Some((_, _, Some(encoded))) = cement_record(node) else {
        return false;
    };
    let Ok(coverage) = serde_json::from_str::<CementCoverage>(encoded) else {
        return false;
    };
    coverage.ids.contains(&node.id)
        && cement_content(atlas, &coverage.ids).is_ok_and(|content| content == coverage.content)
}

/// The CLAIMS section of the read-back. Empty when no node carries a claim.
pub fn describe(atlas: &Atlas) -> String {
    if atlas.claims.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\nCLAIMS (what the agent says it understands, and what the human said back)\n",
    );
    for node in &atlas.nodes {
        let claims = atlas.claims_of(&node.id);
        if claims.is_empty() {
            continue;
        }
        match summary_line(atlas, &node.id) {
            Some(line) => out.push_str(&format!("- {line}\n")),
            None => out.push_str(&format!("- \"{}\": every claim withdrawn\n", node.label)),
        }
        for claim in claims {
            let source = claim.source_ref();
            let basis = if source.is_empty() {
                claim.basis.clone()
            } else {
                format!("{} {source}", claim.basis)
            };
            let standing = if claim.withdrawn {
                "WITHDRAWN by agent".to_string()
            } else {
                match claim.verdict.as_str() {
                    "accepted" => "ACCEPTED by human".to_string(),
                    "rejected" => "REJECTED by human".to_string(),
                    _ => "open".to_string(),
                }
            };
            out.push_str(&format!(
                "    [{}] {basis} \"{}\" {standing}\n",
                claim.id, claim.text
            ));
            for mark in atlas.marks.iter().filter(|mark| mark.target == claim.id) {
                let reply = if mark.answer.is_empty() {
                    "UNANSWERED".to_string()
                } else {
                    format!("answered: {}", mark.answer)
                };
                out.push_str(&format!(
                    "        {} [{}] \"{}\" {reply}\n",
                    mark.glyph, mark.id, mark.text
                ));
            }
        }
        if let Some((path, revision, encoded)) = cement_record(node) {
            if cement_covers(atlas, node) {
                out.push_str(&format!("    CEMENTED as {} at {}\n", path, revision));
            } else {
                out.push_str(&format!("    PREVIOUS CEMENT as {path} at {revision}; current selection is not covered\n"));
            }
            if let Some(coverage) =
                encoded.and_then(|value| serde_json::from_str::<CementCoverage>(value).ok())
            {
                if !coverage.g8_status.is_empty() {
                    out.push_str(&format!("    G8 at adoption: {}. Run g8 check --enforce for current status; structural checks do not prove behavior.\n", coverage.g8_status));
                }
            }
        }
    }
    out
}

/// PROBLEMS sentences the core can measure from the document alone.
pub fn problems(atlas: &Atlas) -> Vec<Problem> {
    let mut out = Vec::new();
    for node in &atlas.nodes {
        if node.status == "agreed" && atlas.under_challenge(&node.id) {
            let open = atlas
                .claims_of(&node.id)
                .iter()
                .filter(|claim| claim.live() && claim.verdict != "accepted")
                .count();
            out.push(Problem {
                code: "claim_conflict",
                ids: vec![node.id.clone()],
                detail: format!(
                    "node \"{}\" is marked agreed but has {} open claim(s)",
                    node.label, open
                ),
            });
        }
    }
    out
}

pub fn digest(atlas: &Atlas) -> Vec<ClaimDigest> {
    atlas
        .claims
        .iter()
        .map(|claim| ClaimDigest {
            id: claim.id.clone(),
            about: claim.about.clone(),
            text: claim.text.clone(),
            basis: claim.basis.clone(),
            source: claim.source_ref(),
            revision: claim.revision.clone(),
            verdict: claim.verdict.clone(),
            withdrawn: claim.withdrawn,
            touched_by: claim.touched_by.clone(),
        })
        .collect()
}

fn standing(digest: &Digest, node_id: &str) -> Option<&'static str> {
    let live: Vec<&ClaimDigest> = digest
        .claims
        .iter()
        .filter(|claim| claim.about == node_id && !claim.withdrawn)
        .collect();
    if live.is_empty() {
        None
    } else if live.iter().all(|claim| claim.verdict == "accepted") {
        Some("SETTLED")
    } else if live.iter().any(|claim| claim.verdict == "rejected") {
        Some("DISAGREED")
    } else if digest
        .marks
        .iter()
        .any(|mark| mark.target == node_id && matches!(mark.glyph.as_str(), "?" | "!"))
    {
        Some("QUESTIONED")
    } else {
        Some("UNREVIEWED")
    }
}

/// A claim target as the CLAIMS section identifies it: its exact text and the
/// node the claim is about. `None` leaves non-claim targets to the caller.
pub(crate) fn target_label(before: &Digest, after: &Digest, id: &str) -> Option<String> {
    let claim = after
        .claims
        .iter()
        .find(|claim| claim.id == id)
        .or_else(|| before.claims.iter().find(|claim| claim.id == id))?;
    let about = after
        .nodes
        .iter()
        .find(|node| node.id == claim.about)
        .or_else(|| before.nodes.iter().find(|node| node.id == claim.about))
        .map(|node| format!("\"{}\"", node.label))
        .unwrap_or_else(|| format!("<gone {}>", claim.about));
    Some(format!("claim \"{}\" about {about}", claim.text))
}

/// CHANGED SINCE YOUR LAST READ sentences for claims.
pub fn changes(before: &Digest, after: &Digest) -> Vec<String> {
    let label_of = |id: &str| -> String {
        after
            .nodes
            .iter()
            .find(|node| node.id == id)
            .or_else(|| before.nodes.iter().find(|node| node.id == id))
            .map(|node| format!("\"{}\"", node.label))
            .unwrap_or_else(|| id.to_string())
    };
    let mut out = Vec::new();
    for claim in &after.claims {
        let on = label_of(&claim.about);
        match before.claims.iter().find(|old| old.id == claim.id) {
            None => out.push(format!(
                "- NEW CLAIM [{}] on {on} ({}): \"{}\"",
                claim.id, claim.basis, claim.text
            )),
            Some(old) => {
                if !old.withdrawn && claim.withdrawn {
                    out.push(format!(
                        "- claim {} on {on} was withdrawn by {}",
                        claim.id, claim.touched_by
                    ));
                    continue;
                }
                if old.verdict != claim.verdict {
                    out.push(match claim.verdict.as_str() {
                        "open" => format!(
                            "- claim {} on {on} was reopened (was {})",
                            claim.id, old.verdict
                        ),
                        verdict => format!(
                            "- claim {} on {on} was {verdict} by {}",
                            claim.id, claim.touched_by
                        ),
                    });
                }
                if old.basis != claim.basis {
                    out.push(format!(
                        "- claim {} on {on} changed basis {} -> {}",
                        claim.id, old.basis, claim.basis
                    ));
                }
                if old.text != claim.text {
                    out.push(format!(
                        "- claim {} on {on} was reworded to \"{}\" (was \"{}\")",
                        claim.id, claim.text, old.text
                    ));
                }
            }
        }
    }
    for old in &before.claims {
        if !after.claims.iter().any(|claim| claim.id == old.id) {
            out.push(format!(
                "- GONE CLAIM [{}] on {} was removed",
                old.id,
                label_of(&old.about)
            ));
        }
    }
    // Standing flips are their own sentence: they are the thing the human
    // is watching for.
    let mut nodes: Vec<&str> = before
        .claims
        .iter()
        .chain(after.claims.iter())
        .map(|claim| claim.about.as_str())
        .collect();
    nodes.sort_unstable();
    nodes.dedup();
    for node in nodes {
        let was = standing(before, node);
        let now = standing(after, node);
        if was != now {
            if let Some(now) = now {
                out.push(format!("- {} is now {now}", label_of(node)));
            }
        }
    }
    out
}

/// Who wrote the claim changes, for the attribution line.
pub fn bylines<'a>(before: &'a Digest, after: &'a Digest) -> Vec<&'a str> {
    let mut out = Vec::new();
    for claim in &after.claims {
        let changed = match before.claims.iter().find(|old| old.id == claim.id) {
            None => true,
            Some(old) => old != claim,
        };
        if changed && !claim.touched_by.is_empty() {
            out.push(claim.touched_by.as_str());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodePatch, describe_changes, mark, place_node, remove};

    fn node(scene: &mut Scene, label: &str) -> String {
        place_node(
            scene,
            &NodePatch {
                label: Some(label.to_string()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("place node")
    }

    fn child_node(scene: &mut Scene, label: &str, parent: &str) -> String {
        place_node(
            scene,
            &NodePatch {
                label: Some(label.to_string()),
                parent: Some(parent.to_string()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("place contained node")
    }

    fn verified(scene: &mut Scene, about: &str, text: &str) -> String {
        claim(
            scene,
            about,
            text,
            "verified",
            "crates/ag-ui-surface/src/activity.rs",
            "187-240",
            &Author::Agent,
        )
        .expect("verified claim")
    }

    #[test]
    fn a_verified_claim_needs_path_and_lines() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        let error = claim(
            &mut scene,
            &n,
            "writes first",
            "verified",
            "",
            "",
            &Author::Agent,
        )
        .expect_err("must refuse");
        assert!(error.contains("needs both path and lines"), "{error}");
        let error = claim(
            &mut scene,
            &n,
            "writes first",
            "verified",
            "a/b.rs",
            "",
            &Author::Agent,
        )
        .expect_err("must refuse");
        assert!(error.contains("needs both path and lines"), "{error}");
    }

    #[test]
    fn an_assumed_or_unknown_claim_refuses_a_source() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        for basis in ["assumed", "unknown"] {
            let error = claim(&mut scene, &n, "x", basis, "a/b.rs", "", &Author::Agent)
                .expect_err("must refuse");
            assert!(error.contains("cannot carry a source"), "{error}");
        }
        claim(&mut scene, &n, "x", "assumed", "", "", &Author::Agent).expect("bare assumed");
        claim(
            &mut scene,
            &n,
            "y",
            "inferred",
            "a/b.rs",
            "",
            &Author::Agent,
        )
        .expect("inferred path");
    }

    #[test]
    fn a_claim_is_about_a_node_only() {
        let mut scene = Scene::new();
        let error = claim(&mut scene, "nope", "x", "unknown", "", "", &Author::Agent)
            .expect_err("missing node");
        assert!(error.contains("claims are about nodes only"), "{error}");
    }

    #[test]
    fn only_the_agent_creates_and_only_the_human_gives_a_verdict() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        let error =
            claim(&mut scene, &n, "x", "unknown", "", "", &Author::Human).expect_err("human");
        assert!(error.contains("only the agent"), "{error}");
        let c = verified(&mut scene, &n, "writes first");
        let error = claim_verdict(&scene, &c, "accepted", &Author::Agent).expect_err("agent");
        assert!(error.contains("only the human"), "{error}");
        let error = claim_verdict(&scene, &c, "accepted", &Author::Named("codex".into()))
            .expect_err("named agent");
        assert!(error.contains("only the human"), "{error}");
        claim_verdict(&scene, &c, "accepted", &Author::Human).expect("human verdict");
        let error = withdraw_claim(&scene, &c, &Author::Human).expect_err("human withdraw");
        assert!(error.contains("only the agent"), "{error}");
        assert_eq!(read(&scene).unwrap().claim(&c).unwrap().verdict, "accepted");
    }

    #[test]
    fn an_unknown_claim_cannot_be_accepted() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        let c = claim(
            &mut scene,
            &n,
            "not known yet",
            "unknown",
            "",
            "",
            &Author::Agent,
        )
        .expect("unknown claim");
        let error = claim_verdict(&scene, &c, "accepted", &Author::Human)
            .expect_err("unknown cannot be accepted");
        assert!(error.contains("unknown basis"), "{error}");
        assert_eq!(read(&scene).unwrap().claim(&c).unwrap().verdict, "open");
    }

    #[test]
    fn revising_an_accepted_claim_reopens_it() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        let c = verified(&mut scene, &n, "writes first");
        claim_verdict(&scene, &c, "accepted", &Author::Human).unwrap();
        assert!(read(&scene).unwrap().settled(&n));
        revise_claim(
            &scene,
            &c,
            "writes first, then acknowledges",
            "inferred",
            "",
            "",
            &Author::Agent,
        )
        .unwrap();
        let atlas = read(&scene).unwrap();
        let claim = atlas.claim(&c).unwrap();
        assert_eq!(claim.verdict, "open");
        assert_eq!(claim.basis, "inferred");
        assert!(atlas.under_challenge(&n));
        assert!(!atlas.settled(&n));
    }

    #[test]
    fn agreed_is_refused_while_under_challenge() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        let c = verified(&mut scene, &n, "writes first");
        let agreed = NodePatch {
            id: Some(n.clone()),
            status: Some("agreed".to_string()),
            ..Default::default()
        };
        let error = place_node(&mut scene, &agreed, &Author::Human).expect_err("refused");
        assert!(error.contains("under challenge"), "{error}");
        assert!(error.contains(&c), "names the open claim: {error}");
        claim_verdict(&scene, &c, "accepted", &Author::Human).unwrap();
        place_node(&mut scene, &agreed, &Author::Human).expect("agreed once settled");
        assert_eq!(read(&scene).unwrap().node(&n).unwrap().status, "agreed");
    }

    #[test]
    fn withdrawing_stops_a_claim_counting() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        let a = verified(&mut scene, &n, "a");
        let b = verified(&mut scene, &n, "b");
        claim_verdict(&scene, &a, "accepted", &Author::Human).unwrap();
        assert!(read(&scene).unwrap().under_challenge(&n));
        withdraw_claim(&scene, &b, &Author::Agent).unwrap();
        let atlas = read(&scene).unwrap();
        assert!(atlas.settled(&n));
        assert_eq!(atlas.claims_of(&n).len(), 2, "withdrawn claims are kept");
        let error = claim_verdict(&scene, &b, "rejected", &Author::Human).expect_err("withdrawn");
        assert!(error.contains("withdrawn"), "{error}");
    }

    #[test]
    fn removing_a_node_removes_its_claims_and_their_marks() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        let c = verified(&mut scene, &n, "a");
        let m = mark(&mut scene, &c, "?", "where?", &Author::Human).expect("mark on claim");
        let atlas = read(&scene).unwrap();
        assert_eq!(atlas.marks[0].target, c);
        assert!(
            atlas.describe().contains("? on \"a\""),
            "mark label is the claim text"
        );
        let removed = remove(&scene, &n).unwrap();
        assert_eq!(removed, 3, "node, claim, mark");
        let atlas = read(&scene).unwrap();
        assert!(atlas.claims.is_empty());
        assert!(!atlas.marks.iter().any(|mark| mark.id == m));
    }

    #[test]
    fn the_claims_section_reads_exactly_as_the_contract_says() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        let c1 = verified(
            &mut scene,
            &n,
            "The journal writes every action to disk before acknowledging it",
        );
        let c2 = claim(
            &mut scene,
            &n,
            "Eviction happens at 800 events",
            "inferred",
            "",
            "",
            &Author::Agent,
        )
        .unwrap();
        let c3 = claim(
            &mut scene,
            &n,
            "Nothing reads the journal but the trace panel",
            "assumed",
            "",
            "",
            &Author::Agent,
        )
        .unwrap();
        claim_verdict(&scene, &c1, "accepted", &Author::Human).unwrap();
        claim_verdict(&scene, &c2, "rejected", &Author::Human).unwrap();
        let m = mark(&mut scene, &c2, "?", "where is 800 set?", &Author::Human).unwrap();
        let described = read(&scene).unwrap().describe();
        let expected = [
            "\nCLAIMS (what the agent says it understands, and what the human said back)\n".to_string(),
            "- \"Persistence\" DISAGREED: 3 claims, 1 open, 1 rejected, 1 accepted\n".to_string(),
            format!("    [{c1}] verified crates/ag-ui-surface/src/activity.rs:187-240 \"The journal writes every action to disk before acknowledging it\" ACCEPTED by human\n"),
            format!("    [{c2}] inferred \"Eviction happens at 800 events\" REJECTED by human\n"),
            format!("        ? [{m}] \"where is 800 set?\" UNANSWERED\n"),
            format!("    [{c3}] assumed \"Nothing reads the journal but the trace panel\" open\n"),
        ]
        .concat();
        assert!(
            described.contains(&expected),
            "section mismatch.\nwanted:\n{expected}\ngot:\n{described}"
        );
        let marks_at = described.find("MARKS (").unwrap();
        let claims_at = described.find("CLAIMS (").unwrap();
        assert!(marks_at < claims_at, "CLAIMS follows MARKS");

        claim_verdict(&scene, &c3, "accepted", &Author::Human).unwrap();
        withdraw_claim(&scene, &c2, &Author::Agent).unwrap();
        let described = read(&scene).unwrap().describe();
        assert!(
            described.contains("- \"Persistence\" SETTLED: 2 claims, all accepted\n"),
            "{described}"
        );
        assert!(described.contains(&format!(
            "[{c2}] inferred \"Eviction happens at 800 events\" WITHDRAWN by agent"
        )));
    }

    #[test]
    fn the_delta_names_verdicts_bases_withdrawals_and_standing() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        let before = read(&scene).unwrap().digest();
        let c = claim(
            &mut scene,
            &n,
            "evicts at 800",
            "inferred",
            "",
            "",
            &Author::Agent,
        )
        .unwrap();
        let d = claim(
            &mut scene,
            &n,
            "writes first",
            "assumed",
            "",
            "",
            &Author::Agent,
        )
        .unwrap();
        let after = read(&scene).unwrap().digest();
        let text = describe_changes(&before, &after);
        assert!(
            text.contains(&format!(
                "- NEW CLAIM [{c}] on \"Persistence\" (inferred): \"evicts at 800\""
            )),
            "{text}"
        );
        assert!(
            text.contains("- \"Persistence\" is now UNREVIEWED"),
            "{text}"
        );
        assert!(
            text.contains("writes by agent"),
            "attributed to the agent: {text}"
        );

        let before = after;
        claim_verdict(&scene, &c, "rejected", &Author::Human).unwrap();
        let after = read(&scene).unwrap().digest();
        let text = describe_changes(&before, &after);
        assert!(
            text.contains(&format!(
                "- claim {c} on \"Persistence\" was rejected by human"
            )),
            "{text}"
        );
        assert!(text.contains("what the human did"), "{text}");

        let before = after;
        revise_claim(
            &scene,
            &c,
            "evicts at 800",
            "verified",
            "a/b.rs",
            "12",
            &Author::Agent,
        )
        .unwrap();
        let after = read(&scene).unwrap().digest();
        let text = describe_changes(&before, &after);
        assert!(
            text.contains(&format!(
                "- claim {c} on \"Persistence\" changed basis inferred -> verified"
            )),
            "{text}"
        );

        let before = after;
        claim_verdict(&scene, &c, "accepted", &Author::Human).unwrap();
        withdraw_claim(&scene, &d, &Author::Agent).unwrap();
        let after = read(&scene).unwrap().digest();
        let text = describe_changes(&before, &after);
        assert!(
            text.contains(&format!(
                "- claim {c} on \"Persistence\" was accepted by human"
            )),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "- claim {d} on \"Persistence\" was withdrawn by agent"
            )),
            "{text}"
        );
        assert!(text.contains("- \"Persistence\" is now SETTLED"), "{text}");
    }

    #[test]
    fn a_mark_on_a_claim_names_the_claim_text_and_node_in_the_delta() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        let c = claim(
            &mut scene,
            &n,
            "The activity journal writes before acknowledging",
            "inferred",
            "",
            "",
            &Author::Agent,
        )
        .expect("claim");
        let before = read(&scene).expect("read before mark").digest();
        let m =
            mark(&mut scene, &c, "?", "Where is the flush?", &Author::Human).expect("mark claim");
        let after = read(&scene).expect("read after mark").digest();

        assert_eq!(
            crate::changes(&before, &after),
            vec![format!(
                "- NEW MARK [{m}] ? on claim \"The activity journal writes before acknowledging\" about \"Persistence\"; the human does not follow it: \"Where is the flush?\""
            )]
        );
    }

    #[test]
    fn an_imported_agreed_node_with_open_claims_is_a_problem() {
        let mut scene = Scene::new();
        let n = place_node(
            &mut scene,
            &NodePatch {
                label: Some("Persistence".into()),
                status: Some("agreed".into()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .unwrap();
        verified(&mut scene, &n, "a");
        let atlas = read(&scene).unwrap();
        let problems = crate::layout(&atlas).problems;
        assert!(
            problems.iter().any(
                |p| p.detail == "node \"Persistence\" is marked agreed but has 1 open claim(s)"
            ),
            "{problems:?}"
        );
    }

    #[test]
    fn a_claim_about_a_missing_node_is_healed_away() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        let c = verified(&mut scene, &n, "a");
        scene
            .set_props(
                &ObjectId::from(c.clone()),
                &[("about", PropValue::Str("ghost".into()))],
            )
            .unwrap();
        let atlas = read(&scene).unwrap();
        assert!(atlas.claims.is_empty());
        assert!(!atlas.under_challenge(&n));
    }

    const REV: &str = "0d6295c9a1b2c3d4e5f60718293a4b5c6d7e8f90";

    #[test]
    fn a_verified_claim_pins_the_commit_its_range_was_read_at() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        let c = claim_at(
            &mut scene,
            &n,
            "writes first",
            "verified",
            "crates/ag-ui-surface/src/activity.rs",
            "187-240",
            REV,
            &Author::Agent,
        )
        .expect("pinned claim");
        let atlas = read(&scene).unwrap();
        let claim = atlas.claim(&c).unwrap();
        assert_eq!(claim.revision, REV);
        assert_eq!(
            claim.source_ref(),
            format!("crates/ag-ui-surface/src/activity.rs:187-240@{REV}")
        );
        let digested = digest(&atlas);
        assert_eq!(digested[0].revision, REV);
        assert_eq!(digested[0].source, claim.source_ref());
        assert!(
            atlas.describe().contains(&format!("187-240@{REV}")),
            "the read-back shows the pin: {}",
            atlas.describe()
        );
        // An unpinned claim reads exactly as it did before revisions existed.
        let bare = verified(&mut scene, &n, "acknowledges after");
        let atlas = read(&scene).unwrap();
        assert_eq!(
            atlas.claim(&bare).unwrap().source_ref(),
            "crates/ag-ui-surface/src/activity.rs:187-240"
        );
    }

    #[test]
    fn a_revision_must_be_a_full_forty_character_hash() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        for bad in ["0d6295c", "not-hex-at-all", &REV[..39], &format!("{REV}0")] {
            let error = claim_at(
                &mut scene,
                &n,
                "writes first",
                "verified",
                "a/b.rs",
                "12",
                bad,
                &Author::Agent,
            )
            .expect_err("must refuse");
            assert!(error.contains("full commit hash"), "{error}");
        }
        claim_at(
            &mut scene,
            &n,
            "writes first",
            "verified",
            "a/b.rs",
            "12",
            &REV.to_ascii_uppercase(),
            &Author::Agent,
        )
        .expect("an uppercase hash is the same hash");
        assert_eq!(read(&scene).unwrap().claims[0].revision, REV);
    }

    #[test]
    fn only_a_verified_claim_can_pin_a_revision() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        let error = claim_at(
            &mut scene,
            &n,
            "evicts at 800",
            "inferred",
            "a/b.rs",
            "12",
            REV,
            &Author::Agent,
        )
        .expect_err("inferred cannot pin");
        assert!(error.contains("cannot pin a revision"), "{error}");
        for basis in ["assumed", "unknown"] {
            let error = claim_at(&mut scene, &n, "x", basis, "", "", REV, &Author::Agent)
                .expect_err("must refuse");
            assert!(error.contains("cannot pin a revision"), "{error}");
        }
    }

    #[test]
    fn revising_a_claim_repins_or_unpins_it() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        let c = verified(&mut scene, &n, "writes first");
        revise_claim_at(
            &scene,
            &c,
            "writes first",
            "verified",
            "a/b.rs",
            "12",
            REV,
            &Author::Agent,
        )
        .unwrap();
        assert_eq!(read(&scene).unwrap().claim(&c).unwrap().revision, REV);
        revise_claim(
            &scene,
            &c,
            "writes first",
            "inferred",
            "a/b.rs",
            "",
            &Author::Agent,
        )
        .unwrap();
        let atlas = read(&scene).unwrap();
        assert_eq!(atlas.claim(&c).unwrap().revision, "");
        assert_eq!(atlas.claim(&c).unwrap().source_ref(), "a/b.rs");
    }

    #[test]
    fn the_badge_words_come_from_the_core() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        assert_eq!(
            read(&scene).unwrap().challenge(&n),
            NodeChallenge::default()
        );

        let a = verified(&mut scene, &n, "a");
        let b = verified(&mut scene, &n, "b");
        let c = verified(&mut scene, &n, "c");
        claim_verdict(&scene, &a, "accepted", &Author::Human).unwrap();
        claim_verdict(&scene, &b, "rejected", &Author::Human).unwrap();
        let atlas = read(&scene).unwrap();
        let standing = atlas.challenge(&n);
        assert_eq!(standing.standing, "open");
        assert_eq!(standing.unsettled, 2);
        assert_eq!(standing.badge, "1 disagreed");
        assert_eq!(
            standing.summary,
            "\"Persistence\" DISAGREED: 3 claims, 1 open, 1 rejected, 1 accepted"
        );
        assert!(
            atlas
                .describe()
                .contains(&format!("- {}\n", standing.summary)),
            "the badge title is the read-back line"
        );

        claim_verdict(&scene, &b, "accepted", &Author::Human).unwrap();
        claim_verdict(&scene, &c, "accepted", &Author::Human).unwrap();
        let atlas = read(&scene).unwrap();
        let standing = atlas.challenge(&n);
        assert_eq!(standing.standing, "settled");
        assert_eq!(standing.unsettled, 0);
        assert_eq!(standing.badge, "settled");
        assert_eq!(
            standing.summary,
            "\"Persistence\" SETTLED: 3 claims, all accepted"
        );

        // Every claim withdrawn is not settled and not under challenge: the
        // node carries nothing to show, so the badge says nothing.
        for id in [&a, &b, &c] {
            withdraw_claim(&scene, id, &Author::Agent).unwrap();
        }
        assert_eq!(
            read(&scene).unwrap().challenge(&n),
            NodeChallenge::default()
        );
    }

    #[test]
    fn accepting_a_revised_claim_does_not_recement_the_old_decision() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        let c = verified(&mut scene, &n, "keeps the journal");
        claim_verdict(&scene, &c, "accepted", &Author::Human).unwrap();
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(n.clone()),
                status: Some("agreed".into()),
                ..Default::default()
            },
            &Author::Human,
        )
        .unwrap();
        record_cemented(
            &scene,
            std::slice::from_ref(&n),
            "docs/decisions/0011-persistence.md",
            "AQIDBA",
        )
        .unwrap();
        revise_claim(
            &scene,
            &c,
            "keeps only recent journal entries",
            "verified",
            "crates/ag-ui-surface/src/activity.rs",
            "187-240",
            &Author::Agent,
        )
        .unwrap();
        assert_eq!(read(&scene).unwrap().challenge(&n).standing, "open");
        claim_verdict(&scene, &c, "accepted", &Author::Human).unwrap();
        let atlas = read(&scene).unwrap();
        assert_eq!(atlas.challenge(&n).standing, "settled");
        assert!(atlas.describe().contains("PREVIOUS CEMENT"));
        assert!(!atlas.describe().contains("    CEMENTED as"));
        record_cemented(
            &scene,
            std::slice::from_ref(&n),
            "docs/decisions/0012-persistence.md",
            "NEW",
        )
        .unwrap();
        assert_eq!(read(&scene).unwrap().challenge(&n).standing, "cemented");
    }

    #[test]
    fn a_cemented_node_has_one_read_back_line_and_cemented_standing() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        let c = verified(&mut scene, &n, "keeps the journal");
        claim_verdict(&scene, &c, "accepted", &Author::Human).unwrap();
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(n.clone()),
                status: Some("agreed".to_string()),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("agree node");

        record_cemented(
            &scene,
            std::slice::from_ref(&n),
            "docs/decisions/0011-persistence.md",
            "AQIDBA",
        )
        .expect("record cement");
        let atlas = read(&scene).unwrap();
        let standing = atlas.challenge(&n);
        assert_eq!(standing.standing, "cemented");
        assert_eq!(standing.badge, "cemented");
        assert!(
            atlas
                .describe()
                .contains("    CEMENTED as docs/decisions/0011-persistence.md at AQIDBA\n")
        );
    }

    fn cement_pair() -> (Scene, Vec<String>, String) {
        let mut scene = Scene::new();
        let mut ids = Vec::new();
        let mut first_claim = String::new();
        for title in ["Persistence", "Journal"] {
            let n = node(&mut scene, title);
            let c = verified(&mut scene, &n, "keeps recent entries");
            if first_claim.is_empty() {
                first_claim = c.clone();
            }
            claim_verdict(&scene, &c, "accepted", &Author::Human).unwrap();
            place_node(
                &mut scene,
                &NodePatch {
                    id: Some(n.clone()),
                    status: Some("agreed".into()),
                    ..Default::default()
                },
                &Author::Human,
            )
            .unwrap();
            ids.push(n);
        }
        record_cemented(&scene, &ids, "docs/decisions/0011-persistence.md", "REV").unwrap();
        (scene, ids, first_claim)
    }

    #[test]
    fn cement_coverage_tracks_the_whole_selection_but_not_geometry_or_outside_edits() {
        let (mut scene, ids, _) = cement_pair();
        node(&mut scene, "Unrelated");
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(ids[0].clone()),
                x: Some(900.0),
                y: Some(700.0),
                ..Default::default()
            },
            &Author::Human,
        )
        .unwrap();
        for id in &ids {
            assert_eq!(read(&scene).unwrap().challenge(id).standing, "cemented");
        }
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(ids[1].clone()),
                note: Some("Changed decision context".into()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .unwrap();
        for id in &ids {
            assert_eq!(read(&scene).unwrap().challenge(id).standing, "settled");
        }
    }

    #[test]
    fn cement_coverage_rejects_changed_claim_basis_source_and_membership() {
        for (key, value) in [
            ("text", "different"),
            ("basis", "inferred"),
            ("path", "another.rs"),
            ("lines", "1-2"),
            ("revision", "0123456789012345678901234567890123456789"),
        ] {
            let (scene, ids, c) = cement_pair();
            // A merged peer can change one field while retaining an accepted
            // verdict. Coverage must derive from content, not setter side effects.
            scene
                .set_props(&ObjectId::from(c), &[(key, PropValue::Str(value.into()))])
                .unwrap();
            for id in &ids {
                assert_eq!(
                    read(&scene).unwrap().challenge(id).standing,
                    "settled",
                    "{key}"
                );
            }
        }
        let (mut scene, ids, _) = cement_pair();
        let extra = verified(&mut scene, &ids[0], "an additional claim");
        claim_verdict(&scene, &extra, "accepted", &Author::Human).unwrap();
        assert_eq!(read(&scene).unwrap().challenge(&ids[1]).standing, "settled");
        let (scene, ids, c) = cement_pair();
        withdraw_claim(&scene, &c, &Author::Agent).unwrap();
        assert_eq!(read(&scene).unwrap().challenge(&ids[1]).standing, "settled");
    }

    #[test]
    fn legacy_cement_is_history_and_cannot_attest_current_content() {
        let (scene, ids, _) = cement_pair();
        scene
            .set_props(
                &ObjectId::from(ids[0].clone()),
                &[(
                    "cemented",
                    PropValue::Str("docs/decisions/0011-persistence.md\nREV".into()),
                )],
            )
            .unwrap();
        let atlas = read(&scene).unwrap();
        assert_eq!(atlas.challenge(&ids[0]).standing, "settled");
        assert!(
            atlas
                .describe()
                .contains("PREVIOUS CEMENT as docs/decisions/0011-persistence.md at REV")
        );
    }

    #[test]
    fn cement_coverage_survives_replication_and_detects_relation_changes() {
        let (scene, ids, _) = cement_pair();
        let replica = Scene::from_state(&scene.encode_full().unwrap()).unwrap();
        assert_eq!(
            read(&replica).unwrap().challenge(&ids[0]).standing,
            "cemented"
        );
        let mut atlas = read(&replica).unwrap();
        atlas.edges.push(crate::Edge {
            id: "new-relation".into(),
            from: ids[0].clone(),
            to: ids[1].clone(),
            label: "now depends on".into(),
            event: String::new(),
            guard: String::new(),
            transition_order: 0,
            created_by: "agent".into(),
            touched_by: "agent".into(),
        });
        for id in &ids {
            assert_eq!(atlas.challenge(id).standing, "settled");
        }
    }

    #[test]
    fn cement_refuses_a_settled_selection_changed_during_publication() {
        let (mut scene, ids, _) = cement_pair();
        let before = cement_content(&read(&scene).unwrap(), &ids).unwrap();
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(ids[1].clone()),
                note: Some("Changed during publication".into()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .unwrap();
        let error = record_cemented_checked(&scene, &ids, "new.md", "NEW", &before).unwrap_err();
        assert!(error.contains("changed while cement was publishing"));
        assert!(!read(&scene).unwrap().describe().contains("new.md"));
    }

    #[test]
    fn a_container_challenge_aggregates_every_descendant_claim() {
        let mut scene = Scene::new();
        let root = node(&mut scene, "Runtime");
        let service = child_node(&mut scene, "Service", &root);
        let worker = child_node(&mut scene, "Worker", &service);
        let outside = node(&mut scene, "Outside");

        let root_accepted = verified(&mut scene, &root, "root accepted");
        let service_rejected = verified(&mut scene, &service, "service rejected");
        let worker_open = verified(&mut scene, &worker, "worker open");
        let worker_accepted = verified(&mut scene, &worker, "worker accepted");
        let worker_withdrawn = verified(&mut scene, &worker, "worker withdrawn");
        verified(&mut scene, &outside, "outside open");
        claim_verdict(&scene, &root_accepted, "accepted", &Author::Human).unwrap();
        claim_verdict(&scene, &service_rejected, "rejected", &Author::Human).unwrap();
        claim_verdict(&scene, &worker_accepted, "accepted", &Author::Human).unwrap();
        withdraw_claim(&scene, &worker_withdrawn, &Author::Agent).unwrap();

        let atlas = read(&scene).unwrap();
        assert_eq!(
            atlas.challenge_within(&root),
            NodeChallenge {
                review_state: "disagreed".into(),
                standing: "open".to_string(),
                unsettled: 2,
                badge: "1 disagreed".to_string(),
                summary: "\"Runtime\" DISAGREED: 4 claims, 1 open, 1 rejected, 2 accepted"
                    .to_string(),
            }
        );
        assert_eq!(atlas.challenge_within(&outside).unsettled, 1);
        assert_eq!(atlas.challenge_within("missing"), NodeChallenge::default());

        claim_verdict(&scene, &service_rejected, "accepted", &Author::Human).unwrap();
        claim_verdict(&scene, &worker_open, "accepted", &Author::Human).unwrap();
        let settled = read(&scene).unwrap().challenge_within(&root);
        assert_eq!(settled.standing, "settled");
        assert_eq!(settled.unsettled, 0);
        assert_eq!(settled.badge, "settled");
        assert_eq!(
            settled.summary,
            "\"Runtime\" SETTLED: 4 claims, all accepted"
        );
    }

    #[test]
    fn a_node_caps_its_live_claims() {
        let mut scene = Scene::new();
        let n = node(&mut scene, "Persistence");
        for i in 0..MAX_CLAIMS_PER_NODE {
            claim(
                &mut scene,
                &n,
                &format!("c{i}"),
                "unknown",
                "",
                "",
                &Author::Agent,
            )
            .unwrap();
        }
        let error = claim(
            &mut scene,
            &n,
            "one more",
            "unknown",
            "",
            "",
            &Author::Agent,
        )
        .unwrap_err();
        assert!(error.contains("two nodes"), "{error}");
    }
}
