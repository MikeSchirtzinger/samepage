//! The neutral record of who did what under whose authority, version 0.1.
//!
//! A [`Record`] is the assembled, durable answer to "who did what under whose
//! authority" that the activity journal alone cannot give. The journal is an
//! operational window (ring-bounded, category-only actors), the CRDT layer has
//! no host-verified identity, and the identity registry is in-memory. This
//! crate fixes those gaps in one shape whose defining rule is that every field
//! group carries a trust label: [`Trust::HostVerified`],
//! [`Trust::ParticipantClaimed`], or [`Trust::Unverifiable`].
//!
//! The validator ([`validate`]) re-checks the record's internal honesty. It is
//! deliberately a library, not a server, and depends on nothing heavier than
//! `serde`, `serde_json`, and `sha2`.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only record version this crate reads. A reader for another version
/// rejects the record before running [`validate`].
pub const RECORD_VERSION: &str = "0.1";

/// How much of a field group the host actually verified.
///
/// `host_verified` means a host route stamped the value from an identity it
/// resolved. `participant_claimed` means a replica asserted it (the CRDT
/// browser replica hardcodes `Author::Human`). `unverifiable` means no durable
/// writer exists, so the field is left absent rather than invented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    HostVerified,
    ParticipantClaimed,
    Unverifiable,
}

impl Trust {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HostVerified => "host_verified",
            Self::ParticipantClaimed => "participant_claimed",
            Self::Unverifiable => "unverifiable",
        }
    }
}

/// The observer that minted the record and its durable journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Session {
    pub journal_id: String,
    pub observer_id: String,
    pub started_at_ms: u64,
    #[serde(default)]
    pub trust: Option<Trust>,
}

/// One actor the record refers to. `actor_id`, `kind`, and `label` are the
/// host-stamped caller category (host_verified). `participant_id`,
/// `principal_key`, and `responsible` are present only when a writer exists for
/// them; in v0.1 records they are absent because no durable writer does, and
/// the record must not vouch for what the host did not verify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Actor {
    pub actor_id: String,
    pub kind: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub participant_id: Option<String>,
    #[serde(default)]
    pub principal_key: Option<String>,
    #[serde(default)]
    pub responsible: Option<String>,
    #[serde(default)]
    pub trust: Option<Trust>,
}

/// Host-stamped identity attached to one activity event (ActivityEvent schema 1
/// verbatim).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityActor {
    pub id: String,
    pub kind: String,
    pub label: String,
}

/// The process that directly witnessed and stamped an activity event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityObserver {
    pub id: String,
    pub kind: String,
    pub label: String,
}

/// Host-generated linkage for one activity event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityCorrelation {
    pub event_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// One extension/state revision sampled by the host around an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityStateRevision {
    pub scope: String,
    pub value: String,
}

/// One normalized, host-stamped occurrence. This is ActivityEvent schema 1
/// verbatim: the field names and category/outcome vocabularies match the
/// journal the runtime already writes, so a real session's events slot in
/// unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEvent {
    pub sequence: u64,
    pub at_ms: u64,
    pub category: String,
    pub kind: String,
    pub actor: ActivityActor,
    pub observer: ActivityObserver,
    pub correlation: ActivityCorrelation,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state_revision_before: Vec<ActivityStateRevision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state_revision_after: Vec<ActivityStateRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Per-category coverage beside the events, as the activity snapshot reports
/// it. `observation` is `observed` or `uncovered`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Coverage {
    pub category: String,
    pub instrumentation: String,
    pub observation: String,
    pub observed_events: usize,
    pub source: String,
    pub note: String,
}

/// The events section. The verbatim [`ActivityEvent`] list sits beside the
/// per-category coverage and one inherited trust label, because the events are
/// the route-dispatched half of the record and every field group must say how
/// much of itself the host verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Events {
    #[serde(default)]
    pub trust: Option<Trust>,
    #[serde(default)]
    pub coverage: Vec<Coverage>,
    pub events: Vec<ActivityEvent>,
}

/// One document revision pair. `scope` is `board` (a u64 revision) or `atlas`
/// (a yrs state vector, URL-safe base64). The CRDT `atlas` document is fixed at
/// [`Trust::ParticipantClaimed`]; the route-dispatched `board` document is
/// fixed at [`Trust::HostVerified`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Document {
    pub scope: String,
    pub revision: Value,
    #[serde(default)]
    pub trust: Option<Trust>,
}

/// One cement bundle settlement: the receipt as the cement step wrote it, plus
/// the obligations document's claimed receipt hash, so the link between the
/// two artifacts can be re-verified without re-reading the obligations file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Settlement {
    pub receipt: Value,
    pub obligations_source_sha256: String,
    #[serde(default)]
    pub trust: Option<Trust>,
}

/// The assembled record. See the module docs for the shape and its rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Record {
    pub record_version: String,
    pub session: Session,
    pub actors: Vec<Actor>,
    pub events: Events,
    pub documents: Vec<Document>,
    pub settlements: Vec<Settlement>,
}

impl Record {
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }
}

/// Which validator check a violation came from. The slugs are the negative
/// fixture filename convention (`neg-<slug>.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    SequenceMonotonicity,
    AtMsSanity,
    ActorReferentialIntegrity,
    CorrelationEventIdUniqueness,
    CoverageHonesty,
    SettlementReverification,
    TrustLabelPresence,
}

impl Check {
    pub const ALL: [Check; 7] = [
        Check::SequenceMonotonicity,
        Check::AtMsSanity,
        Check::ActorReferentialIntegrity,
        Check::CorrelationEventIdUniqueness,
        Check::CoverageHonesty,
        Check::SettlementReverification,
        Check::TrustLabelPresence,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            Check::SequenceMonotonicity => "sequence-monotonicity",
            Check::AtMsSanity => "at-ms-sanity",
            Check::ActorReferentialIntegrity => "actor-referential-integrity",
            Check::CorrelationEventIdUniqueness => "correlation-event-id-uniqueness",
            Check::CoverageHonesty => "coverage-honesty",
            Check::SettlementReverification => "settlement-reverification",
            Check::TrustLabelPresence => "trust-label-presence",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|check| check.slug() == slug)
    }
}

/// One check failure, named so a caller can tell which honesty rule broke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub check: Check,
    pub message: String,
}

/// Run every check and return the violations. The order is stable and each
/// check reports at most one violation, so a fixture that perturbs one field
/// fails exactly the check that owns it.
pub fn validate(record: &Record) -> Vec<Violation> {
    let mut violations = Vec::new();
    sequence_monotonicity(record, &mut violations);
    at_ms_sanity(record, &mut violations);
    actor_referential_integrity(record, &mut violations);
    correlation_event_id_uniqueness(record, &mut violations);
    coverage_honesty(record, &mut violations);
    settlement_reverification(record, &mut violations);
    trust_label_presence(record, &mut violations);
    violations
}

fn sequence_monotonicity(record: &Record, out: &mut Vec<Violation>) {
    let mut previous = 0u64;
    for event in &record.events.events {
        if event.sequence <= previous {
            out.push(Violation {
                check: Check::SequenceMonotonicity,
                message: format!(
                    "event sequence {} is not strictly greater than the previous sequence {previous}",
                    event.sequence
                ),
            });
            return;
        }
        previous = event.sequence;
    }
}

fn at_ms_sanity(record: &Record, out: &mut Vec<Violation>) {
    if record.session.started_at_ms == 0 {
        out.push(Violation {
            check: Check::AtMsSanity,
            message: "session started_at_ms must be positive".to_string(),
        });
        return;
    }
    for event in &record.events.events {
        if event.at_ms == 0 || event.at_ms < record.session.started_at_ms {
            out.push(Violation {
                check: Check::AtMsSanity,
                message: format!(
                    "event sequence {} has at_ms {} before the session start {}",
                    event.sequence, event.at_ms, record.session.started_at_ms
                ),
            });
            return;
        }
    }
}

fn actor_referential_integrity(record: &Record, out: &mut Vec<Violation>) {
    let known: Vec<&str> = record
        .actors
        .iter()
        .map(|actor| actor.actor_id.as_str())
        .collect();
    for event in &record.events.events {
        if !known.contains(&event.actor.id.as_str()) {
            out.push(Violation {
                check: Check::ActorReferentialIntegrity,
                message: format!(
                    "event sequence {} references actor {:?} that is not declared in actors[]",
                    event.sequence, event.actor.id
                ),
            });
            return;
        }
    }
}

fn correlation_event_id_uniqueness(record: &Record, out: &mut Vec<Violation>) {
    let mut seen = HashSet::new();
    for event in &record.events.events {
        if !seen.insert(event.correlation.event_id.as_str()) {
            out.push(Violation {
                check: Check::CorrelationEventIdUniqueness,
                message: format!(
                    "correlation event_id {:?} is attached to more than one event",
                    event.correlation.event_id
                ),
            });
            return;
        }
    }
}

fn coverage_honesty(record: &Record, out: &mut Vec<Violation>) {
    for coverage in &record.events.coverage {
        if coverage.observation == "uncovered" {
            let has_events = record
                .events
                .events
                .iter()
                .any(|event| event.category == coverage.category);
            if has_events || coverage.observed_events != 0 {
                out.push(Violation {
                    check: Check::CoverageHonesty,
                    message: format!(
                        "category {:?} is marked uncovered but carries {} observed events",
                        coverage.category, coverage.observed_events
                    ),
                });
                return;
            }
        }
    }
}

fn settlement_reverification(record: &Record, out: &mut Vec<Violation>) {
    for settlement in &record.settlements {
        if let Err(message) = verify_settlement(settlement) {
            out.push(Violation {
                check: Check::SettlementReverification,
                message,
            });
            return;
        }
    }
}

/// Re-run the cement predicates for one settlement: the sha256 link from the
/// obligations document to the receipt, agree-by-You on every assertion, and
/// the signoff revision covering the authored revision.
fn verify_settlement(settlement: &Settlement) -> Result<(), String> {
    let mut canonical = serde_json::to_vec_pretty(&settlement.receipt)
        .map_err(|error| format!("receipt does not re-serialize: {error}"))?;
    canonical.push(b'\n');
    let recomputed = sha256_hex(&canonical);
    if recomputed != settlement.obligations_source_sha256 {
        return Err(format!(
            "obligations source_sha256 {} does not match the recomputed receipt sha256 {recomputed}",
            settlement.obligations_source_sha256
        ));
    }

    let assertions = settlement
        .receipt
        .get("assertions")
        .and_then(Value::as_array)
        .ok_or_else(|| "receipt has no assertions array".to_string())?;
    for assertion in assertions {
        let status = assertion
            .get("assertion")
            .and_then(|inner| inner.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if status != "settled" {
            return Err(format!("assertion status is {status:?}, not \"settled\""));
        }
        let by = assertion
            .get("signed_off")
            .and_then(|signed| signed.get("by"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if by != "you" {
            return Err(format!("assertion signed_off.by is {by:?}, not \"you\""));
        }
        let mark = assertion
            .get("signed_off")
            .and_then(|signed| signed.get("mark"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if mark != "agree" {
            return Err(format!(
                "assertion signed_off.mark is {mark:?}, not \"agree\""
            ));
        }
        let authored = assertion
            .get("author")
            .and_then(|author| author.get("board_revision"))
            .and_then(Value::as_u64);
        let signed = assertion
            .get("signed_off")
            .and_then(|signed| signed.get("board_revision"))
            .and_then(Value::as_u64);
        match (authored, signed) {
            (Some(authored), Some(signed)) => {
                if signed < authored {
                    return Err(format!(
                        "signoff board_revision {signed} predates authored board_revision {authored}"
                    ));
                }
            }
            _ => {
                return Err(
                    "assertion is missing author.board_revision or signed_off.board_revision"
                        .to_string(),
                )
            }
        }
    }
    Ok(())
}

fn trust_label_presence(record: &Record, out: &mut Vec<Violation>) {
    // Presence: every field group names a trust label. The enum guarantees the
    // value is one of the three; `Option` lets a missing label be reported here
    // instead of as a parse error, so a fixture can fail this check exactly.
    let require = |where_: &str, trust: Option<Trust>, out: &mut Vec<Violation>| -> bool {
        if trust.is_none() {
            out.push(Violation {
                check: Check::TrustLabelPresence,
                message: format!("{where_} is missing its trust label"),
            });
            return true;
        }
        false
    };
    if require("session", record.session.trust, out) {
        return;
    }
    for (index, actor) in record.actors.iter().enumerate() {
        if require(&format!("actors[{index}]"), actor.trust, out) {
            return;
        }
    }
    if require("events", record.events.trust, out) {
        return;
    }
    for (index, document) in record.documents.iter().enumerate() {
        if require(&format!("documents[{index}]"), document.trust, out) {
            return;
        }
    }
    for (index, settlement) in record.settlements.iter().enumerate() {
        if require(&format!("settlements[{index}]"), settlement.trust, out) {
            return;
        }
    }

    // The fixed labels: CRDT authorship is participant_claimed, route-dispatched
    // writes are host_verified. The two document scopes name exactly those two
    // cases.
    for (index, document) in record.documents.iter().enumerate() {
        let expected = match document.scope.as_str() {
            "atlas" => Some(Trust::ParticipantClaimed),
            "board" => Some(Trust::HostVerified),
            _ => None,
        };
        if let (Some(expected), Some(trust)) = (expected, document.trust) {
            if trust != expected {
                out.push(Violation {
                    check: Check::TrustLabelPresence,
                    message: format!(
                        "documents[{index}] scope {:?} must be trust {}, got {}",
                        document.scope,
                        expected.as_str(),
                        trust.as_str()
                    ),
                });
                return;
            }
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_slugs_round_trip_through_the_fixture_convention() {
        for check in Check::ALL {
            assert_eq!(Check::from_slug(check.slug()), Some(check));
        }
        assert_eq!(Check::from_slug("not-a-check"), None);
    }

    #[test]
    fn a_minimal_valid_record_has_no_violations() {
        let record = Record {
            record_version: RECORD_VERSION.to_string(),
            session: Session {
                journal_id: "j".to_string(),
                observer_id: "o".to_string(),
                started_at_ms: 1,
                trust: Some(Trust::HostVerified),
            },
            actors: vec![Actor {
                actor_id: "surface-human".to_string(),
                kind: "human".to_string(),
                label: Some("Human".to_string()),
                participant_id: None,
                principal_key: None,
                responsible: None,
                trust: Some(Trust::HostVerified),
            }],
            events: Events {
                trust: Some(Trust::HostVerified),
                coverage: Vec::new(),
                events: Vec::new(),
            },
            documents: vec![
                Document {
                    scope: "board".to_string(),
                    revision: Value::from(1u64),
                    trust: Some(Trust::HostVerified),
                },
                Document {
                    scope: "atlas".to_string(),
                    revision: Value::from("AQIDBA"),
                    trust: Some(Trust::ParticipantClaimed),
                },
            ],
            settlements: Vec::new(),
        };
        assert!(validate(&record).is_empty(), "{:?}", validate(&record));
    }
}
