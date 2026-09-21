//! Atlas exit: turn a fully signed assertion board into an advisory Govern
//! draft and an inspectable session receipt.
//!
//! This is deliberately example-local. It knows Govern's JSON artifact shape,
//! but it does not call the Govern CLI, store, or extractor and it does not add
//! a runtime dependency. The host supplies the live Atlas CRDT revision; the
//! board supplies host-stamped authorship and human signoff.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};

use crate::assertion::{self, Assertion, Author, Board, Entry, Mark, Status};

use same_page_atlas_core::{Atlas, Claim, Node};

const OBLIGATIONS_FILE: &str = "obligations-v0.1.json";
const RECEIPT_FILE: &str = "cement-receipt.json";
const DECISIONS_DIRECTORY: &str = "docs/decisions";
static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub fn decision_history(root: &Path) -> Result<String, String> {
    let index_path = root.join("docs/decisions/index.json");
    if !index_path.try_exists().map_err(|e| e.to_string())? {
        return Ok("[]".into());
    }
    let index: JsonValue =
        serde_json::from_slice(&fs::read(index_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let canonical = root
        .join("docs/decisions")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let entries = index["decisions"]
        .as_array()
        .ok_or("ADR index has no decisions list")?;
    let mut result = Vec::new();
    for entry in entries.iter().take(256) {
        let relative = entry["path"].as_str().ok_or("ADR path missing")?;
        let file = root
            .join(relative)
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if !file.starts_with(&canonical)
            || fs::metadata(&file).map_err(|e| e.to_string())?.len() > 131072
        {
            return Err("ADR is outside the decision directory or too large to display".into());
        }
        result.push(json!({"id":entry["id"],"title":entry["title"],"status":entry["status"],"path":relative,"body":fs::read_to_string(file).map_err(|e| e.to_string())?}));
    }
    serde_json::to_string(&result).map_err(|e| e.to_string())
}

pub struct CementBundle {
    obligations: JsonValue,
    receipt: JsonValue,
    obligations_bytes: Vec<u8>,
    receipt_bytes: Vec<u8>,
}

/// One ADR, its runnable G8 fragment, and the receipt that binds both to the
/// Atlas revision the human accepted.
#[derive(Clone)]
pub struct DecisionBundle {
    review_token: String,
    adr_path: PathBuf,
    fragment_path: PathBuf,
    adr_bytes: Vec<u8>,
    fragment: JsonValue,
    receipt: JsonValue,
    fragment_bytes: Vec<u8>,
    receipt_bytes: Vec<u8>,
}

struct DecisionNode<'a> {
    node: &'a Node,
    claims: Vec<&'a Claim>,
}

struct VerifiedClaim<'a> {
    claim: &'a Claim,
    node: &'a Node,
    obligation_id: String,
    anchor: String,
}

struct SettledEntry<'a> {
    entry: &'a Entry,
    authored_at: &'a str,
    authored_at_revision: u64,
    signed_off_at: &'a str,
    signed_off_revision: u64,
    note: Option<&'a str>,
}

impl CementBundle {
    /// Return the exact JSON objects the two files would contain, without
    /// touching the filesystem.
    pub fn preview(&self) -> Result<String, String> {
        serde_json::to_string_pretty(&json!({
            "writes": {
                OBLIGATIONS_FILE: self.obligations,
                RECEIPT_FILE: self.receipt,
            }
        }))
        .map_err(|error| format!("could not serialize the cement proposal: {error}"))
    }

    /// Publish both files as one directory rename.
    ///
    /// The human names a NEW directory. Both files are first synced inside a
    /// sibling staging directory and the directory is renamed into place only
    /// when complete. A refusal or write failure therefore cannot leave one
    /// cement artifact pretending the pair exists.
    pub fn write_to_new_directory(&self, directory: &Path) -> Result<[PathBuf; 2], String> {
        let file_name = directory.file_name().ok_or_else(|| {
            format!(
                "cement directory {} must name a new directory, not a filesystem root",
                directory.display()
            )
        })?;
        if directory
            .try_exists()
            .map_err(|error| format!("could not inspect {}: {error}", directory.display()))?
        {
            return Err(format!(
                "cement directory {} already exists; name a new directory so the two artifacts publish atomically",
                directory.display()
            ));
        }

        let parent = directory
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;

        let staging = create_staging_directory(parent, file_name)?;
        let publish = (|| -> Result<(), String> {
            write_new_file(&staging.join(OBLIGATIONS_FILE), &self.obligations_bytes)?;
            write_new_file(&staging.join(RECEIPT_FILE), &self.receipt_bytes)?;
            fs::rename(&staging, directory).map_err(|error| {
                format!(
                    "could not publish cement directory {}: {error}",
                    directory.display()
                )
            })
        })();

        if let Err(error) = publish {
            return match fs::remove_dir_all(&staging) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!(
                    "{error}; additionally could not remove staging directory {}: {cleanup}",
                    staging.display()
                )),
            };
        }

        Ok([
            directory.join(OBLIGATIONS_FILE),
            directory.join(RECEIPT_FILE),
        ])
    }
}

/// Build the two artifacts at one explicit time. Keeping time an input makes
/// the golden fixture byte-for-byte deterministic while the real actions pass
/// the host clock.
pub fn propose_at(
    board: &Board,
    atlas_document_revision: &str,
    cemented_at: &str,
) -> Result<CementBundle, String> {
    if atlas_document_revision.trim().is_empty() {
        return Err("cannot cement without an Atlas document revision".to_string());
    }
    if cemented_at.trim().is_empty() {
        return Err("cannot cement without a timestamp".to_string());
    }

    let settled = settled_entries(board)?;
    let receipt_assertions = settled
        .iter()
        .map(receipt_assertion)
        .collect::<Result<Vec<_>, _>>()?;
    let receipt = json!({
        "schema_version": 1,
        "kind": "same-page-atlas-cement-receipt",
        "cemented_at": cemented_at,
        "atlas_document_revision": atlas_document_revision,
        "board": {
            "revision": board.revision(),
            "subject": board.subject(),
        },
        "assertions": receipt_assertions,
    });
    let receipt_bytes = pretty_bytes(&receipt, RECEIPT_FILE)?;
    let receipt_sha256 = sha256_hex(&receipt_bytes);

    let obligations = settled
        .iter()
        .enumerate()
        .map(|(index, settled)| obligation(settled, index, &receipt_sha256))
        .collect::<Result<Vec<_>, _>>()?;
    let obligations = json!({
        "meta": {
            "artifact": "govern-obligations",
            "version": "0.1.0-draft",
            "status": "draft",
            "source": RECEIPT_FILE,
            "source_sha256": receipt_sha256,
            "atlas_document_revision": atlas_document_revision,
            "board_revision": board.revision(),
            "cemented_at": cemented_at,
            "advisory_only": true,
            "note": "Cemented intent is a review draft. Every obligation remains prose-only and advisory until a reviewer supplies scope and a deterministic checker."
        },
        "obligations": obligations,
        "conflicts": [],
        "open_questions": [],
    });
    let obligations_bytes = pretty_bytes(&obligations, OBLIGATIONS_FILE)?;

    Ok(CementBundle {
        obligations,
        receipt,
        obligations_bytes,
        receipt_bytes,
    })
}

pub fn cement_at(
    board: &Board,
    atlas_document_revision: &str,
    cemented_at: &str,
    directory: &Path,
) -> Result<[PathBuf; 2], String> {
    // Settlement and provenance are checked before the parent directory or a
    // staging path is touched. An unsettled board has no filesystem effect.
    propose_at(board, atlas_document_revision, cemented_at)?.write_to_new_directory(directory)
}

fn settled_entries(board: &Board) -> Result<Vec<SettledEntry<'_>>, String> {
    if board.entries().is_empty() {
        return Err("cannot cement an empty board".to_string());
    }

    let mut settled = Vec::with_capacity(board.entries().len());
    let mut failures = Vec::new();
    for entry in board.entries() {
        match settled_entry(entry) {
            Ok(entry) => settled.push(entry),
            Err(reasons) => {
                failures.push(format!("{} ({})", entry.assertion.id(), reasons.join("; ")))
            }
        }
    }
    if failures.is_empty() {
        Ok(settled)
    } else {
        Err(format!(
            "cannot cement; unsettled assertions: {}",
            failures.join(", ")
        ))
    }
}

fn settled_entry(entry: &Entry) -> Result<SettledEntry<'_>, Vec<String>> {
    let mut reasons = Vec::new();
    let note = match &entry.mark {
        Some((Mark::Agree, note)) => note.as_deref(),
        Some((Mark::Disagree, _)) => {
            reasons.push("marked disagree".to_string());
            None
        }
        Some((mark, _)) => {
            reasons.push(format!("marked {}, not agree", mark.word()));
            None
        }
        None => {
            reasons.push("no human agree mark".to_string());
            None
        }
    };

    let authored_at = required_text(
        entry.authored_at.as_deref(),
        "missing authorship timestamp",
        &mut reasons,
    );
    let authored_at_revision = required_revision(
        entry.authored_at_revision,
        "missing authorship revision",
        &mut reasons,
    );
    match entry.marked_by.as_ref() {
        Some(Author::You) => {}
        Some(author) => reasons.push(format!(
            "signoff is attributed to {}, not the human",
            author.word()
        )),
        None => reasons.push("missing host-stamped signer".to_string()),
    }
    let signed_off_at = required_text(
        entry.marked_at.as_deref(),
        "missing signoff timestamp",
        &mut reasons,
    );
    let signed_off_revision = required_revision(
        entry.marked_at_revision,
        "missing signoff revision",
        &mut reasons,
    );

    if let (Some(authored), Some(signed)) = (authored_at_revision, signed_off_revision) {
        if signed < authored {
            reasons.push(format!(
                "signoff revision {signed} predates authored revision {authored}"
            ));
        }
        if signed != entry.changed_at {
            reasons.push(format!(
                "signoff revision {signed} does not cover current assertion revision {}",
                entry.changed_at
            ));
        }
    }
    if let Assertion::Claim { status, .. } = &entry.assertion {
        if *status != Status::Settled {
            reasons.push(format!(
                "claim status is {}, not settled",
                assertion::status_word(*status)
            ));
        }
    }

    if !reasons.is_empty() {
        return Err(reasons);
    }

    Ok(SettledEntry {
        entry,
        authored_at: authored_at.ok_or_else(|| vec!["missing authorship timestamp".to_string()])?,
        authored_at_revision: authored_at_revision
            .ok_or_else(|| vec!["missing authorship revision".to_string()])?,
        signed_off_at: signed_off_at
            .ok_or_else(|| vec!["missing signoff timestamp".to_string()])?,
        signed_off_revision: signed_off_revision
            .ok_or_else(|| vec!["missing signoff revision".to_string()])?,
        note,
    })
}

fn required_text<'a>(
    value: Option<&'a str>,
    error: &str,
    reasons: &mut Vec<String>,
) -> Option<&'a str> {
    match value.filter(|value| !value.trim().is_empty()) {
        Some(value) => Some(value),
        None => {
            reasons.push(error.to_string());
            None
        }
    }
}

fn required_revision(value: Option<u64>, error: &str, reasons: &mut Vec<String>) -> Option<u64> {
    match value {
        Some(value) => Some(value),
        None => {
            reasons.push(error.to_string());
            None
        }
    }
}

fn receipt_assertion(settled: &SettledEntry<'_>) -> Result<JsonValue, String> {
    let assertion = serde_json::to_value(&settled.entry.assertion).map_err(|error| {
        format!(
            "could not serialize {}: {error}",
            settled.entry.assertion.id()
        )
    })?;
    Ok(json!({
        "id": settled.entry.assertion.id(),
        "kind": settled.entry.assertion.kind_word(),
        "assertion": assertion,
        "author": {
            "byline": settled.entry.author.word(),
            "at": settled.authored_at,
            "board_revision": settled.authored_at_revision,
        },
        "signed_off": {
            "by": "you",
            "at": settled.signed_off_at,
            "board_revision": settled.signed_off_revision,
            "mark": "agree",
            "note": settled.note,
        },
    }))
}

fn obligation(
    settled: &SettledEntry<'_>,
    receipt_index: usize,
    receipt_sha256: &str,
) -> Result<JsonValue, String> {
    let assertion = serde_json::to_value(&settled.entry.assertion).map_err(|error| {
        format!(
            "could not serialize {}: {error}",
            settled.entry.assertion.id()
        )
    })?;
    let id = format!("OBL-ATLAS-{}", settled.entry.assertion.id());
    Ok(json!({
        "id": id,
        "descends_from": {
            "decision": settled.entry.assertion.id(),
            "title": assertion::describe(&settled.entry.assertion),
            "why": format!(
                "Signed off by you at {} on board revision {}.",
                settled.signed_off_at, settled.signed_off_revision
            ),
            "why_kind": "human_signoff",
            "source": {
                "file": RECEIPT_FILE,
                "lines": format!("/assertions/{receipt_index}"),
                "sha256": receipt_sha256,
            }
        },
        "also_stated_by": [],
        "rule": {
            "kind": "atlas_settled_assertion",
            "params": {
                "assertion": assertion,
                "author_byline": settled.entry.author.word(),
                "signed_off_by": "you",
            }
        },
        "scope": {
            "crates": [],
            "paths": [],
        },
        "convergence_test": {
            "id": format!("conv-atlas-{}", settled.entry.assertion.id()),
            "check": "DRAFT: define a deterministic checker before promoting this obligation.",
            "fails_when": "The checker is undefined; prose-only remains unknown rather than passing.",
        },
        "checker": {
            "mode": "prose_only",
        },
        "signal": {
            "gate": "none",
            "conflict_kind": null,
            "wiring": "unwired_draft",
            "advisory": true,
        }
    }))
}

impl DecisionBundle {
    /// Exact proposed artifacts without touching the filesystem.
    pub fn preview(&self) -> Result<String, String> {
        let adr = std::str::from_utf8(&self.adr_bytes)
            .map_err(|error| format!("could not preview the ADR as UTF-8: {error}"))?;
        serde_json::to_string_pretty(&json!({
            "review_token": self.review_token,
            "writes": {
                self.adr_path.to_string_lossy(): adr,
                self.fragment_path.to_string_lossy(): self.fragment,
                RECEIPT_FILE: self.receipt,
            }
        }))
        .map_err(|error| format!("could not serialize the cement proposal: {error}"))
    }

    /// Publish the ADR, fragment, and receipt through one new-directory rename.
    pub fn write_to_new_directory(&self, directory: &Path) -> Result<[PathBuf; 3], String> {
        let file_name = directory.file_name().ok_or_else(|| {
            format!(
                "cement directory {} must name a new directory, not a filesystem root",
                directory.display()
            )
        })?;
        if directory
            .try_exists()
            .map_err(|error| format!("could not inspect {}: {error}", directory.display()))?
        {
            return Err(format!(
                "cement directory {} already exists; name a new directory so all three artifacts publish atomically",
                directory.display()
            ));
        }

        let parent = directory
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        let staging = create_staging_directory(parent, file_name)?;
        let publish = (|| -> Result<(), String> {
            let decisions = staging.join(DECISIONS_DIRECTORY);
            fs::create_dir_all(&decisions)
                .map_err(|error| format!("could not create {}: {error}", decisions.display()))?;
            write_new_file(&staging.join(&self.adr_path), &self.adr_bytes)?;
            write_new_file(&staging.join(&self.fragment_path), &self.fragment_bytes)?;
            write_new_file(&staging.join(RECEIPT_FILE), &self.receipt_bytes)?;
            fs::rename(&staging, directory).map_err(|error| {
                format!(
                    "could not publish cement directory {}: {error}",
                    directory.display()
                )
            })
        })();

        if let Err(error) = publish {
            return match fs::remove_dir_all(&staging) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!(
                    "{error}; additionally could not remove staging directory {}: {cleanup}",
                    staging.display()
                )),
            };
        }

        Ok([
            directory.join(&self.adr_path),
            directory.join(&self.fragment_path),
            directory.join(RECEIPT_FILE),
        ])
    }

    pub fn adr_relative_path(&self) -> &Path {
        &self.adr_path
    }
}

/// Build the Decision step of the Atlas challenge contract.
pub fn propose_decision_at(
    atlas: &Atlas,
    selected_ids: &[String],
    repository_root: &Path,
    atlas_document_revision: &str,
    cemented_at: &str,
) -> Result<DecisionBundle, String> {
    if atlas_document_revision.trim().is_empty() {
        return Err("cannot cement without an Atlas document revision".to_string());
    }
    if cemented_at.trim().is_empty() {
        return Err("cannot cement without a timestamp".to_string());
    }
    let nodes = decision_nodes(atlas, selected_ids)?;
    let slug = slugify(&nodes[0].node.label);
    let number = next_adr_number(repository_root)?;
    let adr_name = format!("{number:04}-{slug}.md");
    let adr_path = PathBuf::from(DECISIONS_DIRECTORY).join(adr_name);
    let fragment_path = PathBuf::from(DECISIONS_DIRECTORY).join(format!("obligations-{slug}.json"));

    let verified = verified_claims(&nodes, repository_root, &slug)?;
    let adr = render_adr(atlas, &nodes, &verified, number, cemented_at, &adr_path);
    let adr_bytes = adr.into_bytes();
    let fragment = render_fragment(&verified, &adr_path, atlas_document_revision, cemented_at);
    let fragment_bytes = pretty_bytes(&fragment, "obligations fragment")?;
    let selected_node_ids = nodes
        .iter()
        .map(|selected| selected.node.id.as_str())
        .collect::<Vec<_>>();
    let accepted_claim_ids = nodes
        .iter()
        .flat_map(|selected| selected.claims.iter().map(|claim| claim.id.as_str()))
        .collect::<Vec<_>>();
    let receipt = json!({
        "schema_version": 1,
        "kind": "same-page-atlas-cement-receipt",
        "cemented_at": cemented_at,
        "atlas_document_revision": atlas_document_revision,
        "adr_path": path_text(&adr_path),
        "obligations_fragment_path": path_text(&fragment_path),
        "selected_node_ids": selected_node_ids,
        "accepted_claim_ids": accepted_claim_ids,
    });
    let receipt_bytes = pretty_bytes(&receipt, RECEIPT_FILE)?;

    Ok(DecisionBundle {
        review_token: String::new(),
        adr_path,
        fragment_path,
        adr_bytes,
        fragment,
        receipt,
        fragment_bytes,
        receipt_bytes,
    })
}

/// Build the reviewed contract. The old source-anchor export remains a legacy
/// fixture format; the live cement action exclusively uses explicit checks.
pub fn propose_reviewed_at(
    atlas: &Atlas,
    selected_ids: &[String],
    draft_id: &str,
    repository_root: &Path,
    revision: &str,
    cemented_at: &str,
) -> Result<DecisionBundle, String> {
    let draft = same_page_atlas_core::decision::reviewed(atlas, draft_id, selected_ids)?;
    let mut bundle =
        propose_decision_at(atlas, selected_ids, repository_root, revision, cemented_at)?;
    let input = &draft.input;
    let spec = repository_root.join("specs/obligations-v0.1.json");
    let previous_spec = if spec.exists() {
        Some(sha256_hex(&fs::read(&spec).map_err(|e| e.to_string())?))
    } else {
        None
    };
    let mut evidence = std::collections::BTreeMap::new();
    let root = repository_root.canonicalize().map_err(|e| e.to_string())?;
    for check in &input.checks {
        for file in &check.evidence_files {
            let candidate = root.join(file);
            if !candidate.exists() {
                evidence.insert(file.clone(), None);
                continue;
            }
            let resolved = candidate
                .canonicalize()
                .map_err(|e| format!("evidence {file}: {e}"))?;
            if !resolved.starts_with(&root) {
                return Err(format!("evidence {file} leaves the repository"));
            }
            evidence.insert(
                file.clone(),
                Some(sha256_hex(&fs::read(resolved).map_err(|e| e.to_string())?)),
            );
        }
    }
    let obligations = input.checks.iter().map(|check| json!({
        "id": check.id,
        "descends_from": {"decision":path_text(&bundle.adr_path),"draft_id":draft.id,"context_ids":draft.context_ids},
        "rule": {"kind":"reviewed_structure","description":check.requirement},
        "checker": {"mode":"typed","checks":[check.check]},
        "signal": {"gate":"cemented_intent","wiring":"repository_artifact","advisory":false}
    })).collect::<Vec<_>>();
    bundle.fragment = json!({"meta":{"artifact":"same-page-reviewed-obligations","authority":path_text(&bundle.adr_path)},"obligations":obligations,"conflicts":[],"open_questions":input.not_enforced});
    bundle.fragment_bytes = pretty_bytes(&bundle.fragment, "reviewed obligations")?;
    let context = atlas
        .context
        .current
        .as_ref()
        .ok_or("decision needs one shared context")?;
    let subjects = context
        .subjects
        .iter()
        .map(|id| {
            atlas
                .node(id)
                .map(|node| node.label.clone())
                .unwrap_or_else(|| id.clone())
        })
        .collect::<Vec<_>>();
    let context_text = format!(
        "\nQuestion: {}\n\nScope: {}\n\nSubjects: {}\n\nAssumptions: {}\n",
        context.question,
        context.altitude.label(),
        if subjects.is_empty() {
            "Whole page".into()
        } else {
            subjects.join(", ")
        },
        if context.assumptions.is_empty() {
            "None recorded"
        } else {
            &context.assumptions
        }
    );
    let mut adr = format!(
        "# {}\n\nStatus: accepted by the human when cemented.\nDate: {}\n\n## Context\n{}\n## Decision\n\n{}\n\n## Why this approach\n\n{}\n\n## Alternatives considered\n\n",
        input.title,
        cemented_at.get(..10).unwrap_or(cemented_at),
        context_text,
        input.decision,
        input.rationale
    );
    for alternative in &input.alternatives {
        adr.push_str(&format!(
            "- {}: {}\n",
            alternative.option, alternative.reason_not_chosen
        ));
    }
    adr.push_str(&format!(
        "\n## Tradeoffs\n\n{}\n\n## Consequences\n\n{}\n\n## Deterministic structural checks\n\n",
        input.tradeoffs, input.consequences
    ));
    for check in &input.checks {
        adr.push_str(&format!("- {}: {}\n", check.id, check.requirement));
    }
    adr.push_str("\nThese checks constrain structure. They do not prove behavioral correctness or human comprehension. Changed evidence requires renewed review.\n\n## Not enforced by these checks\n\n");
    if input.not_enforced.is_empty() {
        adr.push_str("No additional requirements recorded. This is not a claim of complete behavioral coverage.\n");
    }
    for requirement in &input.not_enforced {
        adr.push_str(&format!("- {requirement}\n"));
    }
    adr.push_str("\n## Previously recorded decisions\n\n");
    for node in atlas
        .nodes
        .iter()
        .filter(|node| selected_ids.contains(&node.id) && !node.cemented.is_empty())
    {
        adr.push_str(&format!(
            "- {}: {}\n",
            node.label,
            node.cemented.lines().next().unwrap_or_default()
        ));
    }
    bundle.adr_bytes = adr.into_bytes();
    let index_path = root.join("docs/decisions/index.json");
    let index_hash = if index_path.exists() {
        Some(sha256_hex(
            &fs::read(index_path).map_err(|e| e.to_string())?,
        ))
    } else {
        None
    };
    let review = json!({"draft":draft,"spec_sha256":previous_spec,"evidence":evidence,"adr_path":path_text(&bundle.adr_path),"index_sha256":index_hash,"atlas_revision":revision,"cemented_at":cemented_at,"adr_sha256":sha256_hex(&bundle.adr_bytes),"fragment_sha256":sha256_hex(&bundle.fragment_bytes)});
    bundle.review_token = sha256_hex(&pretty_bytes(&review, "review")?);
    bundle.receipt["review"] = review;
    bundle.receipt["review_token"] = json!(bundle.review_token);
    bundle.receipt["repository"] = json!(root);
    bundle.receipt["adr_sha256"] = json!(sha256_hex(&bundle.adr_bytes));
    bundle.receipt["fragment_sha256"] = json!(sha256_hex(&bundle.fragment_bytes));
    bundle.receipt["g8_status"] =
        json!("pending adoption; ratification activates checks, not a claim they pass");
    bundle.receipt["approval"] = json!({"audience":"human","mechanism":"human-only cement action after review; review_token proves freshness, not authority"});
    bundle.receipt_bytes = pretty_bytes(&bundle.receipt, RECEIPT_FILE)?;
    Ok(bundle)
}

impl DecisionBundle {
    /// Check the exact repository state produced by the adapter immediately
    /// before recording cement in the shared document.
    pub fn verify_adoption_files(&self, root: &Path, adoption: &JsonValue) -> Result<(), String> {
        if adoption["review_token"] != self.review_token {
            return Err("G8 adoption belongs to another review".into());
        }
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let pins = adoption["file_pins"]
            .as_object()
            .ok_or("G8 adoption did not bind its repository files")?;
        for required in [
            "specs/obligations-v0.1.json",
            "docs/decisions/index.json",
            "g8.lock",
        ] {
            if !pins.contains_key(required) {
                return Err(format!("G8 adoption did not bind {required}"));
            }
        }
        for (name, expected) in pins {
            let relative = Path::new(name);
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                return Err("G8 adoption contains a path outside the repository".into());
            }
            let path = root.join(relative);
            let actual = if path.exists() {
                let resolved = path.canonicalize().map_err(|e| e.to_string())?;
                if !resolved.starts_with(&root) {
                    return Err(format!("adopted file leaves the repository: {name}"));
                }
                json!(sha256_hex(&fs::read(resolved).map_err(|e| e.to_string())?))
            } else {
                JsonValue::Null
            };
            if &actual != expected {
                return Err(format!(
                    "repository changed before recording cement: {name}"
                ));
            }
        }
        Ok(())
    }

    pub fn review_token(&self) -> &str {
        &self.review_token
    }
    pub fn reviewed_revision(&self) -> &str {
        self.receipt["review"]["atlas_revision"]
            .as_str()
            .unwrap_or_default()
    }
    pub fn reviewed_draft(&self) -> &str {
        self.receipt["review"]["draft"]["id"]
            .as_str()
            .unwrap_or_default()
    }
    pub fn review_time(&self) -> &str {
        self.receipt["review"]["cemented_at"]
            .as_str()
            .unwrap_or_default()
    }
    pub fn require_freshness(&self, token: &str) -> Result<(), String> {
        if token.is_empty() || token != self.review_token {
            return Err(
                "decision review is stale; preview the current decision before cementing".into(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
pub fn cement_decision_at(
    atlas: &Atlas,
    selected_ids: &[String],
    repository_root: &Path,
    atlas_document_revision: &str,
    cemented_at: &str,
    directory: &Path,
) -> Result<([PathBuf; 3], PathBuf), String> {
    let bundle = propose_decision_at(
        atlas,
        selected_ids,
        repository_root,
        atlas_document_revision,
        cemented_at,
    )?;
    let adr_path = bundle.adr_relative_path().to_path_buf();
    let written = bundle.write_to_new_directory(directory)?;
    Ok((written, adr_path))
}

fn decision_nodes<'a>(
    atlas: &'a Atlas,
    selected_ids: &[String],
) -> Result<Vec<DecisionNode<'a>>, String> {
    if selected_ids.is_empty() {
        return Err("cannot cement an empty Atlas selection".to_string());
    }
    let mut requested = std::collections::BTreeSet::new();
    for id in selected_ids {
        if !requested.insert(id.as_str()) {
            return Err(format!("cannot cement duplicate node id {id:?}"));
        }
        if atlas.node(id).is_none() {
            return Err(format!("cannot cement; no node {id:?} is on the atlas"));
        }
    }

    let mut selected = Vec::with_capacity(selected_ids.len());
    for node in atlas
        .nodes
        .iter()
        .filter(|node| requested.contains(node.id.as_str()))
    {
        if atlas.under_challenge(&node.id) {
            let blocking = atlas
                .claims_of(&node.id)
                .into_iter()
                .find(|claim| claim.live() && claim.verdict != "accepted")
                .ok_or_else(|| format!("node {:?} has inconsistent challenge state", node.label))?;
            return Err(format!(
                "cannot cement node {:?}; it is under challenge because claim {} is {}: {:?}",
                node.label, blocking.id, blocking.verdict, blocking.text
            ));
        }
        if node.status != "agreed" {
            return Err(format!(
                "cannot cement node {:?}; status is {}, not agreed",
                node.label, node.status
            ));
        }
        if !atlas.settled(&node.id) {
            return Err(format!(
                "cannot cement node {:?}; it has no live accepted claims",
                node.label
            ));
        }
        let claims = atlas
            .claims_of(&node.id)
            .into_iter()
            .filter(|claim| claim.live() && claim.verdict == "accepted")
            .collect::<Vec<_>>();
        if let Some(unknown) = claims.iter().find(|claim| claim.basis == "unknown") {
            return Err(format!(
                "cannot cement node {:?}; accepted claim {} has unknown basis",
                node.label, unknown.id
            ));
        }
        selected.push(DecisionNode { node, claims });
    }
    Ok(selected)
}

fn verified_claims<'a>(
    nodes: &[DecisionNode<'a>],
    repository_root: &Path,
    slug: &str,
) -> Result<Vec<VerifiedClaim<'a>>, String> {
    let mut verified = Vec::new();
    for selected in nodes {
        for claim in selected
            .claims
            .iter()
            .copied()
            .filter(|claim| claim.basis == "verified")
        {
            let anchor = source_anchor(repository_root, claim).map_err(|error| {
                format!("verified claim {} source does not hold: {error}", claim.id)
            })?;
            let obligation_id = format!(
                "ATLAS-{}-{:02}",
                slug.to_ascii_uppercase(),
                verified.len() + 1
            );
            verified.push(VerifiedClaim {
                claim,
                node: selected.node,
                obligation_id,
                anchor,
            });
        }
    }
    Ok(verified)
}

fn render_adr(
    atlas: &Atlas,
    nodes: &[DecisionNode<'_>],
    verified: &[VerifiedClaim<'_>],
    number: u32,
    cemented_at: &str,
    _adr_path: &Path,
) -> String {
    let title = nodes
        .iter()
        .map(|selected| selected.node.label.as_str())
        .collect::<Vec<_>>()
        .join(" and ");
    let date = cemented_at.get(..10).unwrap_or(cemented_at);
    let mut out = format!(
        "---\nid: adr-{number:04}\nstatus: accepted\ndate: {date}\nscope: same-page-atlas\ntags: [atlas, cement, g8]\nsupersedes: []\n---\n\n# {title}\n\n## Context\n\n"
    );
    for selected in nodes {
        let node = selected.node;
        out.push_str(&format!(
            "- [{}] {:?} tone={} status={}{}{}\n",
            node.id,
            node.label,
            node.tone,
            node.status,
            if node.source_ref().is_empty() {
                String::new()
            } else {
                format!(" source={}", node.source_ref())
            },
            if node.note.is_empty() {
                String::new()
            } else {
                format!(". {}", node.note)
            }
        ));
    }
    let selected_ids = nodes
        .iter()
        .map(|selected| selected.node.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for edge in atlas.edges.iter().filter(|edge| {
        selected_ids.contains(edge.from.as_str()) && selected_ids.contains(edge.to.as_str())
    }) {
        let from = atlas
            .node(&edge.from)
            .map(|node| node.label.as_str())
            .unwrap_or(edge.from.as_str());
        let to = atlas
            .node(&edge.to)
            .map(|node| node.label.as_str())
            .unwrap_or(edge.to.as_str());
        out.push_str(&format!(
            "- [{}] {:?} -> {:?}{}{}\n",
            edge.id,
            from,
            to,
            if edge.label.is_empty() { "" } else { " : " },
            edge.label
        ));
    }

    out.push_str("\n## Decision\n\n");
    for selected in nodes {
        for claim in &selected.claims {
            let source = claim.source_ref();
            out.push_str(&format!(
                "- [{}] basis={}. {:?}{}\n",
                claim.id,
                claim.basis,
                claim.text,
                if source.is_empty() {
                    " Source: none.".to_string()
                } else {
                    format!(" Source: {source}.")
                }
            ));
        }
    }

    out.push_str("\n## Consequences\n\n");
    if verified.is_empty() {
        out.push_str(
            "- This decision creates no G8 obligation because it has no verified claim.\n",
        );
    } else {
        for item in verified {
            out.push_str(&format!(
                "- {} watches claim {} on {:?} at {}.\n",
                item.obligation_id,
                item.claim.id,
                item.node.label,
                item.claim.source_ref()
            ));
        }
    }
    out
}

fn render_fragment(
    verified: &[VerifiedClaim<'_>],
    adr_path: &Path,
    atlas_document_revision: &str,
    cemented_at: &str,
) -> JsonValue {
    let obligations = verified
        .iter()
        .map(|item| {
            json!({
                "id": item.obligation_id,
                "descends_from": {
                    "decision": path_text(adr_path),
                    "node_id": item.node.id,
                    "claim_id": item.claim.id,
                    "source": {
                        "file": item.claim.path,
                        "lines": item.claim.lines,
                        "revision": item.claim.revision,
                    },
                    "atlas_document_revision": atlas_document_revision,
                },
                "rule": {
                    "kind": "atlas_verified_claim",
                    "params": {
                        "claim": item.claim.text,
                        "basis": item.claim.basis,
                    }
                },
                "checker": {
                    "mode": "typed",
                    "checks": [{
                        "backend": "rg_match_count",
                        "args": {
                            "pattern": format!("^{}$", regex_escape(&item.anchor)),
                            "glob": [item.claim.path],
                            "expected": { "kind": "at_least", "value": 1 }
                        }
                    }]
                },
                "signal": {
                    "gate": "cemented_decision_source",
                    "wiring": "repository_artifact",
                    "advisory": false,
                }
            })
        })
        .collect::<Vec<_>>();
    json!({
        "meta": {
            "artifact": "atlas-cement-obligations-fragment",
            "version": "0.1.0",
            "authority": path_text(adr_path),
            "atlas_document_revision": atlas_document_revision,
            "cemented_at": cemented_at,
            "note": "Merge and ratify this fragment deliberately before treating it as repository enforcement."
        },
        "obligations": obligations,
        "conflicts": [],
        "open_questions": [],
    })
}

fn source_anchor(repository_root: &Path, claim: &Claim) -> Result<String, String> {
    let body = if claim.revision.is_empty() {
        let root = repository_root
            .canonicalize()
            .map_err(|error| format!("repository root is unreadable: {error}"))?;
        let path = root.join(&claim.path);
        let canonical = path
            .canonicalize()
            .map_err(|_| format!("{:?} does not exist in this project", claim.path))?;
        if !canonical.starts_with(&root) {
            return Err(format!("{:?} resolves outside the project", claim.path));
        }
        fs::read_to_string(&canonical)
            .map_err(|error| format!("could not read {:?}: {error}", claim.path))?
    } else {
        let object = format!("{}:{}", claim.revision, claim.path);
        let source = Command::new("git")
            .arg("-C")
            .arg(repository_root)
            .args(["show", "--no-ext-diff", "--no-textconv"])
            .arg(&object)
            .output()
            .map_err(|error| format!("could not run git for {object}: {error}"))?;
        if !source.status.success() {
            return Err(format!("could not read {object}"));
        }
        String::from_utf8(source.stdout).map_err(|_| format!("{object} is not UTF-8 text"))?
    };
    let (start, end) = parse_line_range(&claim.lines)?;
    let lines = body.lines().collect::<Vec<_>>();
    if end > lines.len() {
        return Err(format!(
            "line range {:?} ends past {:?} ({} lines)",
            claim.lines,
            claim.path,
            lines.len()
        ));
    }
    lines[start - 1..end]
        .iter()
        .filter(|line| !line.trim().is_empty())
        .max_by_key(|line| line.len())
        .map(|line| (*line).to_string())
        .ok_or_else(|| {
            format!(
                "line range {:?} in {:?} contains no non-empty line to watch",
                claim.lines, claim.path
            )
        })
}

fn parse_line_range(lines: &str) -> Result<(usize, usize), String> {
    let lines = lines.trim();
    let (start, end) = match lines.split_once('-') {
        Some((start, end)) => (start.trim(), end.trim()),
        None => (lines, lines),
    };
    let start = start
        .parse::<usize>()
        .map_err(|_| format!("line range {lines:?} is not N or N-M"))?;
    let end = end
        .parse::<usize>()
        .map_err(|_| format!("line range {lines:?} is not N or N-M"))?;
    if start == 0 || end < start {
        return Err(format!(
            "line range {lines:?} is not a forward 1-based range"
        ));
    }
    Ok((start, end))
}

fn regex_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(
            character,
            '\\' | '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn slugify(label: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in label.chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(character.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    if slug.is_empty() {
        "decision".to_string()
    } else {
        slug
    }
}

fn next_adr_number(repository_root: &Path) -> Result<u32, String> {
    let decisions = repository_root.join(DECISIONS_DIRECTORY);
    if !decisions.try_exists().map_err(|e| e.to_string())? {
        return Ok(1);
    }
    let mut highest = 0u32;
    for entry in fs::read_dir(&decisions)
        .map_err(|error| format!("could not list {}: {error}", decisions.display()))?
    {
        let entry =
            entry.map_err(|error| format!("could not read {}: {error}", decisions.display()))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some((prefix, _)) = name.split_once('-') else {
            continue;
        };
        if prefix.len() == 4 && prefix.chars().all(|character| character.is_ascii_digit()) {
            if let Ok(number) = prefix.parse::<u32>() {
                highest = highest.max(number);
            }
        }
    }
    highest
        .checked_add(1)
        .ok_or_else(|| "ADR number space is exhausted".to_string())
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn pretty_bytes(value: &JsonValue, name: &str) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("could not serialize {name}: {error}"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn create_staging_directory(
    parent: &Path,
    target_name: &std::ffi::OsStr,
) -> Result<PathBuf, String> {
    let target = target_name.to_string_lossy();
    for _ in 0..32 {
        let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".{target}.atlas-cement-{}-{sequence}",
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "could not create staging directory {}: {error}",
                    path.display()
                ));
            }
        }
    }
    Err(format!(
        "could not allocate a staging directory under {}",
        parent.display()
    ))
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("could not create {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("could not sync {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ag_ui_canvas::scene::{Author as CanvasAuthor, Scene};
    use same_page_atlas_core::{claim, claim_verdict, place_node, read, NodePatch};

    const SETTLED_BOARD: &str = include_str!("../fixtures/cement/settled-board.json");
    const UNSETTLED_BOARD: &str = include_str!("../fixtures/cement/unsettled-board.json");
    const EXPECTED_OBLIGATIONS: &[u8] =
        include_bytes!("../fixtures/cement/expected/obligations-v0.1.json");
    const EXPECTED_RECEIPT: &[u8] =
        include_bytes!("../fixtures/cement/expected/cement-receipt.json");
    const ATLAS_REVISION: &str = "AQIDBA";
    const CEMENTED_AT: &str = "2026-07-31T12:00:00Z";

    fn fixture(value: &str) -> Board {
        serde_json::from_str(value).expect("checked-in board fixture")
    }

    fn temp_target(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "same-page-atlas-cement-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        root.join("cemented")
    }

    fn repository_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("workspace root")
    }

    fn decision_atlas(blocked: bool) -> (Atlas, String, String) {
        let mut scene = Scene::new();
        let node = place_node(
            &mut scene,
            &NodePatch {
                label: Some("Persistence".to_string()),
                ..Default::default()
            },
            &CanvasAuthor::Agent,
        )
        .expect("node");
        let verified = claim(
            &mut scene,
            &node,
            "The challenge contract names the board",
            "verified",
            "docs/design-challenge-v0.md",
            "1-2",
            &CanvasAuthor::Agent,
        )
        .expect("verified claim");
        let inferred = claim(
            &mut scene,
            &node,
            "Settlement is the boundary before a durable decision",
            "inferred",
            "docs/design-challenge-v0.md",
            "",
            &CanvasAuthor::Agent,
        )
        .expect("inferred claim");
        let assumed = claim(
            &mut scene,
            &node,
            "The decision will be reviewed with the repository gate",
            "assumed",
            "",
            "",
            &CanvasAuthor::Agent,
        )
        .expect("assumed claim");
        claim_verdict(
            &scene,
            &verified,
            if blocked { "rejected" } else { "accepted" },
            &CanvasAuthor::Human,
        )
        .expect("verified verdict");
        for id in [&inferred, &assumed] {
            claim_verdict(&scene, id, "accepted", &CanvasAuthor::Human).expect("accepted claim");
        }
        if !blocked {
            place_node(
                &mut scene,
                &NodePatch {
                    id: Some(node.clone()),
                    status: Some("agreed".to_string()),
                    ..Default::default()
                },
                &CanvasAuthor::Human,
            )
            .expect("agree node");
        }
        (read(&scene).expect("atlas"), node, verified)
    }

    #[test]
    fn a_settled_board_cements_into_the_checked_in_golden_artifacts() {
        let target = temp_target("golden");
        let written = cement_at(
            &fixture(SETTLED_BOARD),
            ATLAS_REVISION,
            CEMENTED_AT,
            &target,
        )
        .expect("settled board cements");

        assert_eq!(
            fs::read(&written[0]).expect("obligations written"),
            EXPECTED_OBLIGATIONS
        );
        assert_eq!(
            fs::read(&written[1]).expect("receipt written"),
            EXPECTED_RECEIPT
        );
    }

    #[test]
    fn unsettled_and_disputed_assertions_fail_closed_and_name_every_refusal() {
        let target = temp_target("unsettled");
        let error = cement_at(
            &fixture(UNSETTLED_BOARD),
            ATLAS_REVISION,
            CEMENTED_AT,
            &target,
        )
        .expect_err("an unsettled board is refused");

        assert!(error.contains("open-claim"), "{error}");
        assert!(error.contains("disputed-claim"), "{error}");
        assert!(error.contains("marked disagree"), "{error}");
        assert!(
            !target.exists(),
            "validation happens before any output directory is created"
        );
    }

    #[test]
    fn the_draft_is_explicitly_advisory_and_prose_only() {
        let bundle =
            propose_at(&fixture(SETTLED_BOARD), ATLAS_REVISION, CEMENTED_AT).expect("proposal");
        let obligations = bundle.obligations["obligations"]
            .as_array()
            .expect("obligations array");
        assert_eq!(obligations.len(), 2);
        for obligation in obligations {
            assert_eq!(obligation["checker"]["mode"], "prose_only");
            assert_eq!(obligation["signal"]["advisory"], true);
            assert_eq!(obligation["signal"]["gate"], "none");
        }
    }

    #[test]
    fn atlas_decision_contains_the_adr_fragment_and_receipt_contract() {
        let target = temp_target("atlas-decision");
        let (atlas, node, verified) = decision_atlas(false);
        let revision = "AQIDBA";
        let (written, adr_relative) = cement_decision_at(
            &atlas,
            std::slice::from_ref(&node),
            &repository_root(),
            revision,
            CEMENTED_AT,
            &target,
        )
        .expect("cement Atlas decision");

        let adr = fs::read_to_string(&written[0]).expect("ADR");
        assert!(adr.contains("## Context"), "{adr}");
        assert!(adr.contains("## Decision"), "{adr}");
        assert!(adr.contains("basis=verified"), "{adr}");
        assert!(adr.contains("basis=inferred"), "{adr}");
        assert!(adr.contains("basis=assumed"), "{adr}");
        assert!(adr.contains("## Consequences"), "{adr}");

        let fragment: JsonValue =
            serde_json::from_slice(&fs::read(&written[1]).expect("fragment bytes"))
                .expect("fragment JSON");
        let obligations = fragment["obligations"].as_array().expect("obligations");
        assert_eq!(obligations.len(), 1, "only verified creates an obligation");
        assert_eq!(obligations[0]["descends_from"]["claim_id"], verified);
        assert_eq!(
            obligations[0]["checker"]["checks"][0]["backend"],
            "rg_match_count"
        );
        assert_eq!(
            obligations[0]["checker"]["checks"][0]["args"]["glob"][0],
            "docs/design-challenge-v0.md"
        );

        let receipt: JsonValue =
            serde_json::from_slice(&fs::read(&written[2]).expect("receipt bytes"))
                .expect("receipt JSON");
        assert_eq!(receipt["atlas_document_revision"], revision);
        assert_eq!(receipt["adr_path"], path_text(&adr_relative));
        assert_eq!(
            receipt["obligations_fragment_path"],
            "docs/decisions/obligations-persistence.json"
        );
        assert_eq!(receipt["selected_node_ids"][0], node);
    }

    #[test]
    fn atlas_decision_refuses_an_under_challenge_node_and_names_its_claim() {
        let target = temp_target("atlas-blocked");
        let (atlas, node, blocking_claim) = decision_atlas(true);
        let error = cement_decision_at(
            &atlas,
            &[node],
            &repository_root(),
            ATLAS_REVISION,
            CEMENTED_AT,
            &target,
        )
        .expect_err("under challenge must refuse");

        assert!(error.contains("Persistence"), "{error}");
        assert!(error.contains(&blocking_claim), "{error}");
        assert!(error.contains("rejected"), "{error}");
        assert!(!target.exists(), "a refusal must not create output");
    }
}
