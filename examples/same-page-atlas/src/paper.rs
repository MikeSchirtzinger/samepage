//! Assertions → HTML fragments.
//!
//! This is the only place in the process that makes markup, and every string
//! that came from outside passes through [`escape`] on its way in. The writer
//! sends data; it cannot send a tag. So a `<script>` in a claim is a claim that
//! contains the characters `<script>`, and that stays true whether the writer
//! was the agent, a component, or a person typing into the compose box.
//!
//! The renderer is also where all the presentation decisions live — which is
//! why [`crate::assertion`] has none. This file was rewritten once already,
//! from a grid of cards to a reading document, without a single change to the
//! vocabulary or to anything the agent writes. That is the property the split
//! exists to buy.
//!
//! The shape it renders now is a **review**: an agent has read something and is
//! showing its understanding for approval. So the statement is the loudest
//! thing on the page, metadata is marginal, and the approve/question controls
//! are always present rather than revealed on hover — a control you have to
//! hover to find is a control that disappears when the page updates underneath
//! you.

use std::collections::{BTreeMap, BTreeSet};

use crate::assertion::{status_word, Assertion, Board, Entry, Mark, Verdict};

// The escape is the runtime's now (`ag_ui_surface::html::escape`), not this
// file's. That was the one piece of this renderer worth sharing: every
// server-rendered surface needs it and only needs one.
use ag_ui_surface::html::escape;

/// The whole board, as the fragment htmx swaps into `#board`.
pub fn board(board: &Board) -> String {
    let delta = board.human_delta();
    let fresh: BTreeSet<&str> = delta
        .changed
        .iter()
        .chain(delta.marked.iter())
        .map(String::as_str)
        .collect();

    // Relations render as a clause beneath the statement they start from,
    // because that is where a reader looks for them — not as their own entry.
    let mut outgoing: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut evidence: BTreeMap<&str, Vec<&Entry>> = BTreeMap::new();
    for entry in board.entries() {
        match &entry.assertion {
            Assertion::Relation { from, to, how, .. } => {
                let target = board
                    .get(to)
                    .map(|entry| short(&entry.assertion))
                    .unwrap_or_else(|| to.clone());
                outgoing.entry(from.as_str()).or_default().push(format!(
                    r#"<span class="rel rel-{k}"><i>{kind}</i> {target}</span>"#,
                    k = escape(&how.word().replace(' ', "-")),
                    kind = escape(how.word()),
                    target = escape(&target),
                ));
            }
            Assertion::Evidence { about, .. } => {
                evidence.entry(about.as_str()).or_default().push(entry);
            }
            _ => {}
        }
    }

    let grouped: BTreeSet<&str> = board
        .entries()
        .iter()
        .filter_map(|entry| match &entry.assertion {
            Assertion::Group { members, .. } => Some(members.iter().map(String::as_str)),
            _ => None,
        })
        .flatten()
        .collect();

    let mut out = String::new();
    out.push_str(&summary(board, fresh.len()));

    if board.entries().is_empty() {
        out.push_str(
            r#"<p class="empty">Nothing asserted yet. The agent writes to <code>POST /assert</code>.</p>"#,
        );
        return out;
    }

    for entry in board.entries() {
        if let Assertion::Group { label, members, .. } = &entry.assertion {
            out.push_str(&format!(
                r#"<section class="sec"><h2>{}</h2>"#,
                escape(label)
            ));
            for member in members {
                if let Some(member) = board.get(member) {
                    out.push_str(&statement(member, &outgoing, &evidence, &fresh));
                }
            }
            out.push_str("</section>");
        }
    }

    let loose: Vec<&Entry> = board
        .entries()
        .iter()
        .filter(|entry| {
            !matches!(
                entry.assertion,
                Assertion::Group { .. } | Assertion::Relation { .. } | Assertion::Evidence { .. }
            ) && !grouped.contains(entry.assertion.id())
        })
        .collect();

    if !loose.is_empty() {
        out.push_str(r#"<section class="sec"><h2>Everything else</h2>"#);
        for entry in loose {
            out.push_str(&statement(entry, &outgoing, &evidence, &fresh));
        }
        out.push_str("</section>");
    }

    out
}

/// The standing at the top: how much is waiting on the reader, and what is
/// blocking. A review that does not say what it needs from you is a wall.
fn summary(board: &Board, fresh: usize) -> String {
    let mut awaiting = 0usize;
    let mut agreed = 0usize;
    let mut blocking = 0usize;
    for entry in board.entries() {
        match &entry.assertion {
            Assertion::Question { blocking: true, .. } => blocking += 1,
            Assertion::Question { .. } => {}
            Assertion::Claim { .. } | Assertion::Choice { .. } | Assertion::Decision { .. } => {
                match entry.mark.as_ref().map(|(mark, _)| *mark) {
                    Some(Mark::Agree) => agreed += 1,
                    _ => awaiting += 1,
                }
            }
            _ => {}
        }
    }

    let subject = board
        .subject()
        .map(|subject| format!(r#"<h1>{}</h1>"#, escape(subject)))
        .unwrap_or_else(|| r#"<h1>Understanding</h1>"#.to_string());

    format!(
        r#"<header class="standing">
  {subject}
  <p class="counts">
    <span class="c-await"><b>{awaiting}</b> awaiting you</span>
    <span class="c-ok"><b>{agreed}</b> agreed</span>
    <span class="c-block"><b>{blocking}</b> blocking</span>
    <span class="c-rev" data-rev="{rev}">rev {rev}</span>
    {new}
  </p>
</header>"#,
        rev = board.revision(),
        new = if fresh == 0 {
            String::new()
        } else {
            format!(r#"<span class="c-new">{fresh} new since you looked</span>"#)
        }
    )
}

/// A one-line stand-in used when a relation names another assertion.
fn short(assertion: &Assertion) -> String {
    let text = match assertion {
        Assertion::Claim { text, .. }
        | Assertion::Question { text, .. }
        | Assertion::Choice { text, .. } => text.clone(),
        Assertion::Group { label, .. } => label.clone(),
        Assertion::Decision { chose, .. } => format!("decision: {chose}"),
        Assertion::Evidence { source, .. } => source.clone(),
        Assertion::Relation { how, .. } => how.word().to_string(),
    };
    if text.chars().count() > 72 {
        let clipped: String = text.chars().take(69).collect();
        format!("{clipped}…")
    } else {
        text
    }
}

/// One statement, as a paragraph with a margin rather than a box.
fn statement(
    entry: &Entry,
    outgoing: &BTreeMap<&str, Vec<String>>,
    evidence: &BTreeMap<&str, Vec<&Entry>>,
    fresh: &BTreeSet<&str>,
) -> String {
    let id = entry.assertion.id();
    let kind = entry.assertion.kind_word();

    // The sentence itself — the largest text on the page.
    let (lead, aside) = match &entry.assertion {
        Assertion::Claim { text, .. } => (escape(text), String::new()),
        Assertion::Question { text, .. } => (escape(text), String::new()),
        Assertion::Choice { text, tradeoff, .. } => (
            escape(text),
            match tradeoff {
                Some(tradeoff) => {
                    format!(r#"<p class="aside"><i>costs</i> {}</p>"#, escape(tradeoff))
                }
                None => r#"<p class="aside none"><i>no tradeoff stated</i></p>"#.to_string(),
            },
        ),
        Assertion::Decision {
            chose,
            over,
            because,
            ..
        } => (
            format!(
                "Chose <b>{}</b>{}",
                escape(chose),
                if over.is_empty() {
                    String::new()
                } else {
                    format!(
                        r#" <span class="over">over {}</span>"#,
                        escape(&over.join(", "))
                    )
                }
            ),
            format!(r#"<p class="aside"><i>because</i> {}</p>"#, escape(because)),
        ),
        Assertion::Evidence { source, .. } => (escape(source), String::new()),
        Assertion::Group { label, .. } => (escape(label), String::new()),
        Assertion::Relation { how, .. } => (escape(how.word()), String::new()),
    };

    // Everything that is not the sentence lives on one quiet line under it.
    let mut tags: Vec<String> = vec![format!(
        r#"<span class="kind k-{k}">{k}</span>"#,
        k = escape(kind)
    )];
    if let Assertion::Claim { status, .. } = &entry.assertion {
        tags.push(format!(
            r#"<span class="st st-{s}">{s}</span>"#,
            s = escape(status_word(*status))
        ));
    }
    if let Assertion::Question { blocking: true, .. } = &entry.assertion {
        tags.push(r#"<span class="st st-blocking">blocking</span>"#.to_string());
    }
    tags.push(format!(
        r#"<span class="by by-{b}">{b}</span>"#,
        b = escape(entry.author.word())
    ));
    tags.push(format!(r#"<code class="id">{}</code>"#, escape(id)));

    let rels = outgoing
        .get(id)
        .map(|items| format!(r#"<p class="rels">{}</p>"#, items.join("")))
        .unwrap_or_default();

    let cited = evidence
        .get(id)
        .map(|entries| {
            let rows: String = entries
                .iter()
                .filter_map(|entry| match &entry.assertion {
                    Assertion::Evidence {
                        source, verdict, ..
                    } => Some(format!(
                        r#"<p class="ev ev-{v}"><i>{v}</i><code>{s}</code></p>"#,
                        v = escape(match verdict {
                            Verdict::Confirmed => "confirmed",
                            Verdict::Refuted => "refuted",
                            Verdict::Unverified => "unverified",
                        }),
                        s = escape(source)
                    )),
                    _ => None,
                })
                .collect();
            rows
        })
        .unwrap_or_default();

    let mark = entry
        .mark
        .as_ref()
        .map(|(mark, note)| {
            format!(
                r#"<p class="yours m-{w}"><span class="g">{g}</span> <i>you marked this {w}</i>{note}</p>"#,
                w = escape(mark.word()),
                g = escape(mark.glyph()),
                note = note
                    .as_ref()
                    .map(|note| format!(" — {}", escape(note)))
                    .unwrap_or_default()
            )
        })
        .unwrap_or_default();

    let marked = entry.mark.as_ref().map(|(mark, _)| *mark);

    format!(
        r#"<article class="stmt s-{kind}{new}{done}" id="card-{id}">
  <div class="gutter">{controls}</div>
  <div class="body">
    <p class="lead">{lead}</p>
    {aside}
    {rels}
    {cited}
    {mark}
    <p class="tags">{tags}</p>
  </div>
</article>"#,
        kind = escape(kind),
        new = if fresh.contains(id) { " is-new" } else { "" },
        done = if marked == Some(Mark::Agree) {
            " is-agreed"
        } else {
            ""
        },
        id = escape(id),
        tags = tags.join(""),
        controls = controls(id, marked),
    )
}

/// The reader's controls. Always visible — a control revealed on hover is one
/// the reader cannot find, and one that vanishes if the page updates while the
/// pointer is moving toward it.
///
/// These post to `/mark`, which has no agent-audience twin: a `✓` is always
/// something a person put there.
fn controls(id: &str, current: Option<Mark>) -> String {
    let id = escape(id);
    let mut out = String::new();
    for (value, glyph, title) in [
        ("agree", "✓", "Agreed — this matches what I think"),
        ("question", "?", "I have a question about this"),
        ("disagree", "✗", "This is wrong"),
    ] {
        let active = current.map(|mark| mark.word() == value).unwrap_or(false);
        let (path, vals) = if active {
            ("/board/paper/unmark", format!(r#"{{"id":"{id}"}}"#))
        } else {
            (
                "/board/paper/mark",
                format!(r#"{{"id":"{id}","mark":"{value}"}}"#),
            )
        };
        // `r##"…"##`: the markup contains `hx-target="#board"`, and the `"#`
        // in that would close an ordinary `r#"…"#` literal mid-attribute.
        out.push_str(&format!(
            r##"<button class="ctl c-{value}{on}" title="{title}" hx-post="{path}" hx-vals='{vals}' hx-target="#board" hx-swap="innerHTML">{glyph}</button>"##,
            on = if active { " on" } else { "" }
        ));
    }
    out
}

/// The catalog — the vocabulary describing itself, rendered from the same
/// source the parser uses.
pub fn primitives(catalog: &serde_json::Value) -> String {
    let mut out = String::from(r#"<div class="catalog">"#);
    out.push_str(&format!(
        r#"<p class="note">{}</p>"#,
        escape(catalog["note"].as_str().unwrap_or_default())
    ));
    if let Some(items) = catalog["assertions"].as_array() {
        for item in items {
            let kind = item["kind"].as_str().unwrap_or_default();
            out.push_str(&format!(
                r#"<article class="prim"><header><span class="kind k-{k}">{k}</span><span class="says">{says}</span></header>"#,
                k = escape(kind),
                says = escape(item["says"].as_str().unwrap_or_default())
            ));
            if let Some(fields) = item["fields"].as_object() {
                out.push_str(r#"<dl class="fields">"#);
                for (name, ty) in fields {
                    out.push_str(&format!(
                        r#"<dt>{}</dt><dd>{}</dd>"#,
                        escape(name),
                        escape(ty.as_str().unwrap_or_default())
                    ));
                }
                out.push_str("</dl>");
            }
            if let Some(notes) = item["notes"].as_str() {
                out.push_str(&format!(r#"<p class="notes">{}</p>"#, escape(notes)));
            }
            if let Some(example) = item.get("example") {
                out.push_str(&format!(
                    r#"<pre class="example">{}</pre>"#,
                    escape(&serde_json::to_string_pretty(example).unwrap_or_default())
                ));
            }
            out.push_str("</article>");
        }
    }
    out.push_str("</div>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assertion::{Assertion, Author, Board, Status};

    #[test]
    fn a_tag_in_an_assertion_is_text_not_markup() {
        let mut b = Board::default();
        b.assert(
            Assertion::Claim {
                id: "x".to_string(),
                text: "<script>alert(1)</script>".to_string(),
                status: Status::Proposed,
            },
            Author::Agent,
        )
        .expect("claim");
        let html = board(&b);
        assert!(
            !html.contains("<script>"),
            "a hostile string reached the page as markup"
        );
        assert!(
            html.contains("&lt;script&gt;"),
            "the text should still be readable, escaped"
        );
    }

    #[test]
    fn presentation_lives_here_and_nowhere_else() {
        let mut b = Board::default();
        for id in ["a", "b"] {
            b.assert(
                Assertion::Claim {
                    id: id.to_string(),
                    text: id.to_string(),
                    status: Status::Proposed,
                },
                Author::Agent,
            )
            .expect("claim");
        }
        b.assert(
            Assertion::Relation {
                id: "r".to_string(),
                from: "a".to_string(),
                to: "b".to_string(),
                how: crate::assertion::RelationKind::Supports,
            },
            Author::Agent,
        )
        .expect("r");
        let html = board(&b);
        assert!(html.contains("rel-supports"), "the relation should render");
        assert!(
            !html.contains(r#"id="card-r""#),
            "a relation is drawn on its source, not as its own entry"
        );
    }

    #[test]
    fn the_reader_controls_are_not_hidden_behind_hover() {
        // The previous renderer put these behind `opacity: 0` until hover,
        // which combined badly with a page that reswaps itself. Keep them in
        // the markup unconditionally so the regression is visible here.
        let mut b = Board::default();
        b.assert(
            Assertion::Claim {
                id: "a".to_string(),
                text: "a".to_string(),
                status: Status::Proposed,
            },
            Author::Agent,
        )
        .expect("claim");
        let html = board(&b);
        assert!(html.contains(r#"class="gutter""#), "controls must render");
        assert!(
            html.contains("hx-post=\"/board/paper/mark\""),
            "agree must be reachable"
        );
    }

    #[test]
    fn agreeing_again_takes_the_mark_off() {
        // The control is a toggle: pressing ✓ on something already agreed
        // posts to /unmark, so a reader can undo without hunting for a
        // separate clear button.
        let mut b = Board::default();
        b.assert(
            Assertion::Claim {
                id: "a".to_string(),
                text: "a".to_string(),
                status: Status::Proposed,
            },
            Author::Agent,
        )
        .expect("claim");
        b.mark("a", Mark::Agree, None).expect("agree");
        let html = board(&b);
        assert!(
            html.contains("hx-post=\"/board/paper/unmark\""),
            "an already-agreed statement should offer to undo"
        );
    }
}
