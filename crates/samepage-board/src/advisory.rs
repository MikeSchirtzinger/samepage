//! Advisory export of a settled board; never activates enforcement.
use crate::assertion::{self, Assertion, Author, Board, Entry, Mark, Status};
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
pub const OBLIGATIONS_FILE: &str = "obligations-v0.1.json";
pub const RECEIPT_FILE: &str = "cement-receipt.json";
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CementBundle {
    obligations: JsonValue,
    receipt: JsonValue,
    obligations_bytes: Vec<u8>,
    receipt_bytes: Vec<u8>,
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

    pub fn files(&self) -> [(&str, &[u8]); 2] {
        [
            (OBLIGATIONS_FILE, &self.obligations_bytes),
            (RECEIPT_FILE, &self.receipt_bytes),
        ]
    }
}
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
