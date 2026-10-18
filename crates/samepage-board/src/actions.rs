//! One catalog shared by the native dispatcher and the WASM descriptor.
use samepage_capability::{ActionDecl, Audience, Effect};
use serde_json::{Value as JsonValue, json};
fn assertion_schema() -> JsonValue {
    json!({
        "type": "object",
        "properties": {
            "kind": {
                "type": "string",
                "enum": ["claim", "question", "choice", "relation", "group", "evidence", "decision"]
            },
            "id": {
                "type": "string",
                "description": "your name for this assertion; writing the same id again replaces it, and a human mark on it survives"
            },
            "text": { "type": "string", "description": "claim / question / choice: what is being said" },
            "status": { "type": "string", "enum": ["proposed", "supported", "contested"], "description": "claim only. `settled` is not yours to write — only a human ✓ settles a claim" },
            "blocking": { "type": "boolean", "description": "question only: work stops until it is answered" },
            "tradeoff": { "type": "string", "description": "choice only: what it costs. A choice with no tradeoff is a preference in disguise" },
            "from": { "type": "string", "description": "relation only: the id it starts at" },
            "to": { "type": "string", "description": "relation only: the id it points at" },
            "how": { "type": "string", "enum": ["supports", "contradicts", "depends_on", "refines", "answers"], "description": "relation only" },
            "label": { "type": "string", "description": "group only: what the set is called" },
            "members": { "type": "array", "items": { "type": "string" }, "description": "group only: ids in the set" },
            "about": { "type": "string", "description": "evidence only: the id it bears on" },
            "source": { "type": "string", "description": "evidence only: what you actually checked — a command and its result, a file and its lines" },
            "verdict": { "type": "string", "enum": ["unverified", "confirmed", "refuted"], "description": "evidence only; defaults to unverified, because citing is not checking" },
            "chose": { "type": "string", "description": "decision only: the id that won" },
            "over": { "type": "array", "items": { "type": "string" }, "description": "decision only: the ids that lost" },
            "because": { "type": "string", "description": "decision only, required: why. A decision without it is unreviewable later" }
        },
        "required": ["kind", "id"],
        "additionalProperties": false
    })
}

fn action(
    name: &str,
    audience: Audience,
    effect: Effect,
    required: &[&str],
    properties: JsonValue,
    description: &str,
) -> ActionDecl {
    let mut a = ActionDecl::new(name, audience, effect, description);
    a.input_schema = json!({"type":"object","properties":properties,"required":required,"additionalProperties":false});
    a
}
pub fn declarations() -> Vec<ActionDecl> {
    use Audience::{Agent, Human};
    use Effect::{Mutate, Query};
    let empty = json!({});
    let id = json!({"id":{"type":"string"}});
    let wait = json!({"seconds":{"type":"integer","minimum":1,"maximum":600}});
    vec![
 action("board_read",Agent,Query,&[],empty.clone(),"Read the board first: assertions, bylines, human marks, stale sign-offs, and changes since this participant last read."),
 action("board_revision",Agent,Query,&[],empty.clone(),"Inspect the current board revision without consuming the read cursor."),
 action("await_board",Agent,Query,&[],wait.clone(),"Wait for board changes or runtime chat, then read the board. Default 60 seconds, maximum 600."),
 action("await_input",Agent,Query,&[],wait,"Wait for the next thing another participant does: a card, mark, claim or drawing change, a board question, what a person selects, chat in the conversation panel, or a new set of page problems. Returns what changed; for chat, call chat_read then chat_reply. Page problems wake you once per new set and never block the wait. Default 60 seconds, at most 600; call again to keep waiting."),
 action("board_assert",Agent,Mutate,&["assertions"],json!({"assertions":{"type":"array","minItems":1,"items":assertion_schema()}}),"Write assertions in order. Relations can use earlier ids. A semantic refusal publishes the successful prefix and names what landed. Rewriting keeps human marks but makes old sign-off stale."),
 action("board_retract",Agent,Mutate,&["id"],id.clone(),"Retract an assertion and its dangling dependents."),
 action("board_subject",Agent,Mutate,&["subject"],json!({"subject":{"type":"string","minLength":1,"maxLength":4000}}),"Name the board's subject."),
 action("board_cement_preview",Agent,Query,&[],empty.clone(),"Preview the advisory settled-board bundle. Writes nothing; no enforcement or gate activation.").alias("atlas_cement_propose"),
 action("board_compose",Human,Mutate,&["kind","text"],json!({"kind":{"enum":["claim","question"]},"text":{"type":"string","maxLength":4000}}),"Write a claim or question in your own name. The host mints its id."),
 action("board_mark",Human,Mutate,&["id","mark"],json!({"id":{"type":"string"},"mark":{"enum":["question","important","agree","disagree"]},"note":{"type":"string","maxLength":4000}}),"Put a human question, emphasis, agreement or disagreement on an assertion. Only a fresh human agreement settles it."),
 action("board_unmark",Human,Mutate,&["id"],id,"Withdraw a human mark and its settlement."),
 action("board_seen",Human,Mutate,&[],empty.clone(),"Acknowledge the board; a repaint never advances this session cursor."),
 action("board_clear",Human,Mutate,&[],empty.clone(),"Clear the board; the revision remains monotonic."),
 action("board_cement",Human,Mutate,&["directory"],json!({"directory":{"type":"string","minLength":1}}),"Publish both advisory settled-board artifacts into a new directory. No enforcement or gate activation.").alias("atlas_cement"),
 action("board_snapshot",Human,Query,&[],empty,"Inspect the native board snapshot for portable WASM rendering, without consuming any cursor."),
 ]
}
