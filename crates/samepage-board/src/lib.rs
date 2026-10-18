//! Portable board state and rules. The board is a native LWW JSON store, not
//! a CRDT kind. The standard ABI supports projections of transient snapshots;
//! state transitions have an explicit JSON store interface below. Neither
//! interface authenticates callers: that is the native host's job.
mod actions;
pub mod advisory;
pub mod assertion;
pub use assertion::{
    Assertion, Author, Board, Delta as BoardDelta, Entry, Mark, Status, fresh_agreement,
};
use samepage_capability::{
    Artifact, Call, CapError, Capability, Delta, Descriptor, LegendEntry, Plan, Record, Trigger,
    View,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
pub const ID: &str = "board";
pub const VERSION: &str = "0.1.0";
pub fn descriptor() -> Descriptor {
    let mut d = Descriptor::new(ID, VERSION, Artifact::Wasm);
    for action in actions::declarations() {
        d = d.trigger(Trigger::Tool {
            name: action.name.clone(),
        });
        for alias in &action.aliases {
            d = d.trigger(Trigger::Tool {
                name: alias.clone(),
            });
        }
        d = d.action(action);
    }
    d.trigger(Trigger::Route {
        name: "/board".into(),
    })
}
pub fn preferred(name: &str) -> Option<String> {
    descriptor()
        .actions
        .into_iter()
        .find(|a| a.name == name || a.aliases.iter().any(|n| n == name))
        .map(|a| a.name)
}
fn invalid(message: impl Into<String>) -> CapError {
    CapError::new("invalid", message)
}
/// Data supplied by the native adapter after authentication. A browser may
/// call this pure function with any data, but cannot publish its outcome.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub author: Author,
    pub participant: String,
    pub timestamp: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub board: Board,
    pub written: Vec<String>,
    pub readback: String,
    pub error: Option<String>,
}
fn string<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} must be text"))
}
/// Malformed input refuses before any writes; semantic errors retain the
/// successful prefix. The native adapter must publish it even on error.
pub fn transition_at(board: &Board, call: &Call, ctx: &Context) -> Result<Outcome, CapError> {
    let action =
        preferred(&call.action).ok_or_else(|| CapError::new("unknown_action", &call.action))?;
    let mut next = board.clone();
    let mut written = Vec::new();
    if ctx.timestamp.trim().is_empty() || ctx.participant.trim().is_empty() {
        return Err(invalid("host timestamp and participant are required"));
    }
    let declaration = descriptor()
        .actions
        .into_iter()
        .find(|a| a.name == action)
        .unwrap();
    if declaration.audience == samepage_capability::Audience::Human && ctx.author != Author::You {
        return Err(CapError::new(
            "human_context_required",
            "a human transition needs host-supplied human context",
        ));
    }
    if declaration.audience == samepage_capability::Audience::Agent && ctx.author == Author::You {
        return Err(CapError::new(
            "agent_context_required",
            "an agent transition needs host-supplied agent context",
        ));
    }
    let args = call
        .args
        .as_object()
        .ok_or_else(|| invalid("arguments must be an object"))?;
    if args
        .keys()
        .any(|k| declaration.input_schema["properties"].get(k).is_none())
    {
        return Err(invalid("unknown argument"));
    }
    for key in declaration.input_schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !args.contains_key(key) {
            return Err(invalid(format!("missing argument {key}")));
        }
    }
    let mut parsed = Vec::new();
    if action == "board_assert" {
        let items = call.args["assertions"]
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| invalid("assertions must be a nonempty array"))?;
        // Parsing every item first preserves the old malformed-JSON contract.
        for item in items {
            parsed.push(
                serde_json::from_value::<Assertion>(item.clone())
                    .map_err(|e| invalid(format!("{item} is not a valid assertion: {e}")))?,
            );
        }
    }
    let result = (|| -> Result<String, String> {
        match action.as_str() {
            "board_assert" => {
                for assertion in parsed {
                    let id = assertion.id().to_string();
                    let message = next
                        .assert_at(assertion, ctx.author.clone(), ctx.timestamp.clone())
                        .map_err(|e| format!("{id}: {e}"))?;
                    next.bind_author(&id, &ctx.participant);
                    written.push(message);
                }
                Ok(format!(
                    "{}. The human can mark or answer any of it.",
                    written.join(", ")
                ))
            }
            "board_compose" => {
                let kind = string(&call.args, "kind")?;
                let text = string(&call.args, "text")?.to_string();
                let mut seq = next.revision() + 1;
                let mut id = format!("you-{seq}");
                while next.get(&id).is_some() {
                    seq += 1;
                    id = format!("you-{seq}");
                }
                let assertion = match kind {
                    "claim" => Assertion::Claim {
                        id: id.clone(),
                        text,
                        status: Status::Proposed,
                    },
                    "question" => Assertion::Question {
                        id: id.clone(),
                        text,
                        blocking: false,
                    },
                    _ => return Err("compose kind must be claim or question".into()),
                };
                let result = next.assert_at(assertion, Author::You, ctx.timestamp.clone())?;
                next.bind_author(&id, &ctx.participant);
                Ok(result)
            }
            "board_mark" => {
                let id = string(&call.args, "id")?;
                let mark = Mark::parse(string(&call.args, "mark")?)?;
                let note = match call.args.get("note") {
                    Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
                    None | Some(Value::String(_)) => None,
                    _ => return Err("note must be text".into()),
                };
                let result = next.mark_at(id, mark, note, ctx.timestamp.clone())?;
                next.bind_signer(id, &ctx.participant);
                Ok(result)
            }
            "board_unmark" => next.unmark(string(&call.args, "id")?),
            "board_retract" => next.retract(string(&call.args, "id")?),
            "board_subject" => {
                let subject = string(&call.args, "subject")?;
                if subject.trim().is_empty() || subject.len() > 4000 {
                    return Err("subject must be 1..=4000 characters".into());
                }
                next.set_subject(subject.into());
                Ok("subject named".into())
            }
            "board_clear" => Ok(next.clear()),
            other => Err(format!(
                "{other} is native session/query work, not a board transition"
            )),
        }
    })();
    let (readback, error) = match result {
        Ok(message) => (message, None),
        Err(error) => (
            String::new(),
            Some(if written.is_empty() {
                error
            } else {
                format!("{error}. Already written: {}", written.join(", "))
            }),
        ),
    };
    Ok(Outcome {
        board: next,
        written,
        readback,
        error,
    })
}
/// A transient projection input only; it is never applied to the shared doc.
pub fn snapshot_view(board: &Board) -> View {
    snapshot_view_for(board, None)
}

pub fn snapshot_view_for(board: &Board, reader: Option<&str>) -> View {
    View::new(
        board.revision().to_string(),
        vec![
            Record::new("board.snapshot", "board-snapshot")
                .with(
                    "json",
                    serde_json::to_string(board).expect("board serializes"),
                )
                .with("reader", reader.unwrap_or_default()),
        ],
    )
    .expect("snapshot record")
}
fn from_view(view: &View) -> Result<Board, CapError> {
    let raw=view.get("board.snapshot").and_then(|r|r.props.get("json")).and_then(|s|s.as_str()).ok_or_else(||CapError::new("board_snapshot_required","board lives in the native JSON store; use a transient board snapshot for projection"))?;
    serde_json::from_str(raw).map_err(|e| invalid(e.to_string()))
}
fn reader_from_view(view: &View) -> Option<&str> {
    view.get("board.snapshot")?
        .props
        .get("reader")?
        .as_str()
        .filter(|s| !s.is_empty())
}
pub struct BoardCapability;
impl Capability for BoardCapability {
    fn descriptor(&self) -> Descriptor {
        descriptor()
    }
    fn project(&self, view: &View) -> Result<Value, CapError> {
        Ok(project_for(&from_view(view)?, 0, reader_from_view(view)))
    }
    fn legend(&self, _view: &View) -> Result<Vec<LegendEntry>, CapError> {
        Ok([
            (
                "standing",
                "SETTLED",
                "Fresh human sign-off",
                "board-card",
                "✓ SETTLED",
            ),
            (
                "element",
                "stale",
                "Agreement predates these words",
                "board-stale",
                "STALE SIGN-OFF",
            ),
        ]
        .into_iter()
        .map(|(attribute, value, label, class, symbol)| LegendEntry {
            capability: ID.into(),
            attribute: attribute.into(),
            value: value.into(),
            label: label.into(),
            class: Some(class.into()),
            symbol: Some(symbol.into()),
        })
        .collect())
    }
    fn digest(&self, view: &View) -> Result<Value, CapError> {
        Ok(json!({"board":from_view(view)?}))
    }
    fn plan(&self, call: &Call, view: &View) -> Result<Plan, CapError> {
        let board = from_view(view)?;
        let name =
            preferred(&call.action).ok_or_else(|| CapError::new("unknown_action", &call.action))?;
        let readback = match name.as_str() {
            "board_read" => board.render_for(0, reader_from_view(view)),
            "board_revision" => board.revision().to_string(),
            "board_snapshot" => {
                serde_json::to_string(&board).map_err(|e| invalid(e.to_string()))?
            }
            _ => {
                return Err(CapError::new(
                    "native_store_required",
                    "board transitions publish through the native LWW adapter, never document ops; use transition with host-supplied context",
                ));
            }
        };
        Ok(Plan {
            ops: vec![],
            readback,
        })
    }
    fn validate(&self, _delta: &Delta, view: &View) -> Result<(), CapError> {
        validate_state(&from_view(view)?)
    }
    fn changes(
        &self,
        before: &Value,
        after: &Value,
        reader: &str,
    ) -> Result<Vec<String>, CapError> {
        let a: Board =
            serde_json::from_value(before["board"].clone()).map_err(|e| invalid(e.to_string()))?;
        let b: Board =
            serde_json::from_value(after["board"].clone()).map_err(|e| invalid(e.to_string()))?;
        let d = b.delta_since(a.revision());
        Ok(d.changed
            .iter()
            .map(|id| format!("written: {id}"))
            .chain(b.marked_changes(&d.marked, Some(reader)))
            .collect())
    }
}
pub fn validate_state(board: &Board) -> Result<(), CapError> {
    let mut ids = std::collections::BTreeSet::new();
    for entry in board.entries() {
        assertion::validate(&entry.assertion).map_err(invalid)?;
        if !ids.insert(entry.assertion.id()) {
            return Err(invalid("duplicate board id"));
        }
        if entry.changed_at > board.revision() {
            return Err(invalid("entry revision is ahead of board"));
        }
        for reference in entry.assertion.references() {
            if reference == entry.assertion.id() || board.get(reference).is_none() {
                return Err(invalid(format!(
                    "{} has dangling reference {reference}",
                    entry.assertion.id()
                )));
            }
        }
    }
    Ok(())
}
pub fn project(board: &Board, since: u64) -> Value {
    project_for(board, since, None)
}

pub fn project_for(board: &Board, since: u64, reader: Option<&str>) -> Value {
    let delta = board.delta_since(since);
    let entries:Vec<_>=board.entries().iter().map(|entry|{
        let mut value=serde_json::to_value(&entry.assertion).expect("assertion serializes");
        value["author"]=json!(entry.author.word()); value["author_participant"]=json!(entry.author_participant);
        value["authored_at"]=json!(entry.authored_at); value["authored_at_revision"]=json!(entry.authored_at_revision); value["changed_at"]=json!(entry.changed_at);
        value["mark"]=entry.mark.as_ref().map(|(m,n)|json!({"as":m.word(),"glyph":m.glyph(),"note":n,"by":board.marker_byline(entry, reader),"participant":entry.marked_by_participant,"at":entry.marked_at,"board_revision":entry.marked_at_revision})).unwrap_or(Value::Null);
        value["fresh_agreement"]=json!(fresh_agreement(entry));
        value["standing"]=json!(if fresh_agreement(entry){"SETTLED"}else if entry.mark.as_ref().is_some_and(|(m,_)|*m==Mark::Agree){"STALE SIGN-OFF"}else{"OPEN"});
        value
    }).collect();
    json!({"revision":board.revision(),"subject":board.subject(),"seen_at":since,"unseen":delta.changed.iter().chain(&delta.marked).collect::<Vec<_>>(),"entries":entries})
}
fn response<T: Serialize>(result: Result<T, CapError>) -> String {
    match result {
        Ok(value) => json!({"abi":samepage_capability::ABI,"ok":value}).to_string(),
        Err(error) => json!({"abi":samepage_capability::ABI,"error":error}).to_string(),
    }
}
fn decode<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, CapError> {
    serde_json::from_str(raw).map_err(|e| CapError::new("bad_input", e.to_string()))
}
#[cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-export"),
    wasm_bindgen::prelude::wasm_bindgen
)]
pub fn transition(board_json: &str, call_json: &str, context_json: &str) -> String {
    response((|| {
        transition_at(
            &decode(board_json)?,
            &decode(call_json)?,
            &decode(context_json)?,
        )
    })())
}
#[cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-export"),
    wasm_bindgen::prelude::wasm_bindgen
)]
pub fn project_board(board_json: &str, since_json: &str) -> String {
    response((|| Ok(project(&decode(board_json)?, decode(since_json)?)))())
}
#[cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-export"),
    wasm_bindgen::prelude::wasm_bindgen
)]
pub fn project_board_for(board_json: &str, since_json: &str, reader_json: &str) -> String {
    response((|| {
        Ok(project_for(
            &decode(board_json)?,
            decode(since_json)?,
            decode::<Option<String>>(reader_json)?.as_deref(),
        ))
    })())
}
#[cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-export"),
    wasm_bindgen::prelude::wasm_bindgen
)]
pub fn validate_board(board_json: &str) -> String {
    response((|| validate_state(&decode(board_json)?))())
}
#[cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-export"),
    wasm_bindgen::prelude::wasm_bindgen
)]
pub fn delta(board_json: &str, since_json: &str) -> String {
    response((|| {
        Ok(decode::<Board>(board_json)?.delta_since(decode(since_json)?))
    })())
}
#[cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-export"),
    wasm_bindgen::prelude::wasm_bindgen
)]
pub fn migrate(board_json: &str) -> String {
    response(decode::<Board>(board_json))
}
#[cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-export"),
    wasm_bindgen::prelude::wasm_bindgen
)]
pub fn readback(board_json: &str, since_json: &str) -> String {
    response((|| {
        Ok(decode::<Board>(board_json)?.render(decode(since_json)?))
    })())
}
#[cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-export"),
    wasm_bindgen::prelude::wasm_bindgen
)]
pub fn readback_for(board_json: &str, since_json: &str, reader_json: &str) -> String {
    response((|| {
        Ok(decode::<Board>(board_json)?.render_for(
            decode(since_json)?,
            decode::<Option<String>>(reader_json)?.as_deref(),
        ))
    })())
}
#[cfg(all(target_arch = "wasm32", feature = "wasm-export"))]
samepage_capability::export_capability!(BoardCapability, BoardCapability);

#[cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-export"),
    wasm_bindgen::prelude::wasm_bindgen
)]
pub fn advisory_preview(board_json: &str, revision: &str, timestamp: &str) -> String {
    response((|| {
        advisory::propose_at(&decode(board_json)?, revision, timestamp)
            .map_err(invalid)?
            .preview()
            .map_err(invalid)
    })())
}
