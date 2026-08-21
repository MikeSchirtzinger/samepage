//! The room's view vocabulary.
//!
//! A pane's contents are a tree of these nodes — data, never markup. The agent
//! writes the tree; a trusted browser module is the only thing that ever makes
//! a DOM node out of it. That is the whole reason this vocabulary exists: the
//! room has to be able to become an arbitrary interface *during* a
//! conversation, and "let the model emit HTML" is not a way to do that safely.
//!
//! Two node kinds do not carry their own content. `Source` names a file and a
//! line range; `Options` names nothing at all. The host resolves both at
//! snapshot time in [`resolve`], so a source excerpt is whatever the file says
//! *now* rather than whatever it said when the agent wrote the pane. Reviewing
//! code together and editing it in the same session therefore agree.

use ag_ui_surface::diagram;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value as JsonValue};

use crate::catalog::Workspace;

/// Natural layout units → SVG pixels, and the margin around the drawing. The
/// pane scales the finished `view_box` to its own width, so these only set the
/// diagram's internal proportions, never its size on screen.
const DIAGRAM_SCALE: f64 = 34.0;
const DIAGRAM_PAD: f64 = 12.0;

pub const MAX_NODES: usize = 400;
pub const MAX_DEPTH: usize = 8;
const MAX_TEXT: usize = 8_000;
const MAX_CODE: usize = 24_000;
const MAX_HTML: usize = 48_000;
const MAX_LABEL: usize = 160;
const MAX_ASK: usize = 2_000;
const MAX_ITEMS: usize = 200;
const MAX_COLUMNS: usize = 8;
const MAX_URL: usize = 2_000;

/// Presentation intent, not a colour. The browser maps these onto the active
/// theme so a pane written under one theme still reads correctly under another.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    #[default]
    Neutral,
    Muted,
    Strong,
    Accent,
    Good,
    Warn,
    Bad,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct KvItem {
    pub label: String,
    pub value: String,
}

/// One box in a [`Node::Diagram`]. `id` is what edges refer to; `label` is what
/// the reader sees, and defaults to the id. Tone rather than colour, for the
/// same reason every other node uses tone: a diagram drawn under one theme has
/// to stay readable under the other.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DiagramNode {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub tone: Tone,
}

/// One directed connection between two [`DiagramNode`] ids.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DiagramEdge {
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrow: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Node {
    /// Vertical container.
    Stack {
        #[serde(default)]
        children: Vec<Node>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<u8>,
    },
    /// Horizontal container; wraps by default so a narrow pane stays readable.
    Row {
        #[serde(default)]
        children: Vec<Node>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<u8>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wrap: Option<bool>,
    },
    /// Shows one child at a time, with the reader flipping between them.
    /// Which child is showing is each reader's own — like scroll position,
    /// it is never shared state and never round-trips through the agent.
    Deck {
        #[serde(default)]
        children: Vec<Node>,
        /// One label per child for the flip counter, or empty for "n of N".
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        titles: Vec<String>,
    },
    Heading {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        level: Option<u8>,
    },
    Text {
        text: String,
        #[serde(default)]
        tone: Tone,
    },
    /// Inert monospace text. `lang` is a label shown to the reader; nothing
    /// highlights or executes it.
    Code {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lang: Option<String>,
    },
    List {
        items: Vec<String>,
        #[serde(default)]
        ordered: bool,
    },
    /// Label/value pairs — the shape most status readouts actually want.
    Kv {
        items: Vec<KvItem>,
    },
    Table {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Badge {
        text: String,
        #[serde(default)]
        tone: Tone,
    },
    Divider,
    /// Sends `ask` to the agent as if the person had typed it. Any `field`
    /// values in the same pane are appended, which is how a pane becomes a
    /// form without the vocabulary needing a form node.
    Button {
        label: String,
        ask: String,
        #[serde(default)]
        tone: Tone,
    },
    Field {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        #[serde(default)]
        multiline: bool,
    },
    /// Opens the URL in a new tab. To put a site *inside* the room, use `embed`.
    Link {
        label: String,
        url: String,
    },
    /// Same-origin path or `data:` URI only — the room does not fetch remote
    /// images on the person's behalf.
    Image {
        src: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alt: Option<String>,
    },
    /// Host-resolved file excerpt, re-read on every snapshot.
    Source {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<u32>,
    },
    /// Host-resolved catalog of everything this workspace can run.
    Options {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<String>,
    },
    /// A site, framed inside the room. Sandboxed, and an agent-authored embed
    /// does not load until the person clicks it.
    Embed {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
    },
    /// Agent-authored HTML in a fully isolated sandbox. Scripts may run
    /// inside; nothing inside can reach this page. The host injects a pointer
    /// bridge, so what the person clicks is reported as the pane's note and
    /// lands in read-back — the shared referent survives arbitrary markup.
    ///
    /// This is the no-build escape hatch: an interface the vocabulary has no
    /// word for yet is tried here first, and earns a typed node only if it
    /// proves worth keeping.
    Html {
        html: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
    },
    /// A graph: boxes joined by arrows.
    ///
    /// The agent sends *structure* — nodes and directed edges — and never
    /// coordinates. The host lays it out at snapshot time via the shared
    /// [`ag_ui_surface::diagram`] brain and resolves placed geometry into the
    /// tree, exactly as [`Node::Source`] resolves a file excerpt. That keeps
    /// the layout in one tested Rust module instead of a second
    /// implementation in the renderer, and keeps the agent out of the
    /// business of inventing x/y — which it does badly and slowly.
    Diagram {
        nodes: Vec<DiagramNode>,
        #[serde(default)]
        edges: Vec<DiagramEdge>,
        /// `"right"` (roots left, flow rightward — pipelines, computation
        /// graphs) or `"down"` (roots on top). Defaults to `"right"`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direction: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
}

impl Node {
    fn children(&self) -> &[Node] {
        match self {
            Node::Stack { children, .. }
            | Node::Row { children, .. }
            | Node::Deck { children, .. } => children,
            _ => &[],
        }
    }

    /// The word used for this node in read-back and in error messages.
    fn label(&self) -> &'static str {
        match self {
            Node::Stack { .. } => "stack",
            Node::Row { .. } => "row",
            Node::Deck { .. } => "deck",
            Node::Heading { .. } => "heading",
            Node::Text { .. } => "text",
            Node::Code { .. } => "code",
            Node::List { .. } => "list",
            Node::Kv { .. } => "kv",
            Node::Table { .. } => "table",
            Node::Badge { .. } => "badge",
            Node::Divider => "divider",
            Node::Button { .. } => "button",
            Node::Field { .. } => "field",
            Node::Link { .. } => "link",
            Node::Image { .. } => "image",
            Node::Source { .. } => "source",
            Node::Options { .. } => "options",
            Node::Embed { .. } => "embed",
            Node::Html { .. } => "html",
            Node::Diagram { .. } => "diagram",
        }
    }
}

/// Collect every `html` node's markup, in document order. The pane-html route
/// serves the nth entry as its own document, so this walk and the renderer's
/// pre-walk must agree on the ordering — both visit every `children` array
/// depth-first, including deck faces that are not currently showing.
pub fn collect_html<'a>(node: &'a Node, found: &mut Vec<&'a str>) {
    if let Node::Html { html, .. } = node {
        found.push(html);
    }
    for child in node.children() {
        collect_html(child, found);
    }
}

/// Reject a tree the browser should never be asked to render. Every limit here
/// is a limit the renderer then does not have to defend itself against.
pub fn validate(root: &Node) -> Result<(), String> {
    let mut counted = 0usize;
    check(root, 1, &mut counted)
}

fn check(node: &Node, depth: usize, counted: &mut usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!("view nests deeper than {MAX_DEPTH} levels"));
    }
    *counted += 1;
    if *counted > MAX_NODES {
        return Err(format!("view has more than {MAX_NODES} nodes"));
    }

    match node {
        Node::Stack { children, .. } | Node::Row { children, .. } => {
            if children.is_empty() {
                return Err(format!("a {} needs at least one child", node.label()));
            }
        }
        Node::Deck { children, titles } => {
            if children.is_empty() {
                return Err(format!("a {} needs at least one child", node.label()));
            }
            if !titles.is_empty() && titles.len() != children.len() {
                return Err(format!(
                    "a deck with titles needs one per child: {} titles for {} children",
                    titles.len(),
                    children.len()
                ));
            }
            for title in titles {
                bound("deck title", title, 1, MAX_LABEL)?;
            }
        }
        Node::Heading { text, level } => {
            bound("heading text", text, 1, MAX_LABEL)?;
            if let Some(level) = level {
                if !(1..=3).contains(level) {
                    return Err("heading level must be 1, 2 or 3".to_string());
                }
            }
        }
        Node::Text { text, .. } => bound("text", text, 1, MAX_TEXT)?,
        Node::Code { text, lang } => {
            bound("code", text, 1, MAX_CODE)?;
            if let Some(lang) = lang {
                bound("code lang", lang, 1, 32)?;
            }
        }
        Node::List { items, .. } => {
            cardinality("list items", items.len(), MAX_ITEMS)?;
            for item in items {
                bound("list item", item, 1, MAX_TEXT)?;
            }
        }
        Node::Kv { items } => {
            cardinality("kv items", items.len(), MAX_ITEMS)?;
            for item in items {
                bound("kv label", &item.label, 1, MAX_LABEL)?;
                bound("kv value", &item.value, 0, MAX_TEXT)?;
            }
        }
        Node::Table { columns, rows } => {
            cardinality("table columns", columns.len(), MAX_COLUMNS)?;
            if columns.is_empty() {
                return Err("a table needs at least one column".to_string());
            }
            cardinality("table rows", rows.len(), MAX_ITEMS)?;
            for column in columns {
                bound("table column", column, 0, MAX_LABEL)?;
            }
            for (index, row) in rows.iter().enumerate() {
                if row.len() != columns.len() {
                    return Err(format!(
                        "table row {index} has {} cells but the table declares {} columns",
                        row.len(),
                        columns.len()
                    ));
                }
                for cell in row {
                    bound("table cell", cell, 0, MAX_TEXT)?;
                }
            }
        }
        Node::Badge { text, .. } => bound("badge text", text, 1, MAX_LABEL)?,
        Node::Divider => {}
        Node::Diagram {
            nodes,
            edges,
            direction,
            caption,
        } => {
            if nodes.is_empty() {
                return Err("a diagram needs at least one node".to_string());
            }
            cardinality("diagram nodes", nodes.len(), MAX_ITEMS)?;
            cardinality("diagram edges", edges.len(), MAX_ITEMS)?;
            for node in nodes {
                bound("diagram node id", &node.id, 1, MAX_LABEL)?;
                if let Some(label) = &node.label {
                    bound("diagram node label", label, 0, MAX_LABEL)?;
                }
            }
            for edge in edges {
                bound("diagram edge from", &edge.from, 1, MAX_LABEL)?;
                bound("diagram edge to", &edge.to, 1, MAX_LABEL)?;
                if let Some(label) = &edge.label {
                    bound("diagram edge label", label, 0, MAX_LABEL)?;
                }
            }
            if let Some(direction) = direction {
                bound("diagram direction", direction, 1, 16)?;
            }
            if let Some(caption) = caption {
                bound("diagram caption", caption, 0, MAX_LABEL)?;
            }
            // Everything structural — unknown edge endpoints, duplicate ids,
            // self-edges — is the shared spec's judgement, so the room and the
            // canvas reject the same mistakes with the same words.
            spec_of(nodes, edges, direction.as_deref()).map(|_| ())?;
        }
        Node::Button { label, ask, .. } => {
            bound("button label", label, 1, MAX_LABEL)?;
            bound("button ask", ask, 1, MAX_ASK)?;
        }
        Node::Field {
            key,
            label,
            placeholder,
            ..
        } => {
            bound("field key", key, 1, 48)?;
            if !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                return Err(format!(
                    "field key {key:?} may only contain letters, digits, '_' and '-'"
                ));
            }
            if let Some(label) = label {
                bound("field label", label, 0, MAX_LABEL)?;
            }
            if let Some(placeholder) = placeholder {
                bound("field placeholder", placeholder, 0, MAX_LABEL)?;
            }
        }
        Node::Link { label, url } => {
            bound("link label", label, 1, MAX_LABEL)?;
            web_url(url)?;
        }
        Node::Image { src, alt } => {
            bound("image src", src, 1, MAX_URL)?;
            let same_origin = src.starts_with('/')
                && !src.starts_with("//")
                && !src.contains("..")
                && !src.contains('\\');
            if !(same_origin || src.starts_with("data:image/")) {
                return Err(format!(
                    "image src {src:?} must be a same-origin absolute path or a data:image/ URI"
                ));
            }
            if let Some(alt) = alt {
                bound("image alt", alt, 0, MAX_LABEL)?;
            }
        }
        Node::Source { path, from, to } => {
            bound("source path", path, 1, 400)?;
            if let (Some(from), Some(to)) = (from, to) {
                if from > to {
                    return Err(format!("source range {from}-{to} ends before it starts"));
                }
            }
            if matches!(from, Some(0)) {
                return Err("source lines are 1-based".to_string());
            }
        }
        Node::Options { filter } => {
            if let Some(filter) = filter {
                bound("options filter", filter, 0, MAX_LABEL)?;
            }
        }
        Node::Embed { url, height } => {
            web_url(url)?;
            if let Some(height) = height {
                if !(120..=2_000).contains(height) {
                    return Err("embed height must be between 120 and 2000 pixels".to_string());
                }
            }
        }
        Node::Html { html, height } => {
            bound("html", html, 1, MAX_HTML)?;
            if let Some(height) = height {
                if !(120..=2_000).contains(height) {
                    return Err("html height must be between 120 and 2000 pixels".to_string());
                }
            }
        }
    }

    for child in node.children() {
        check(child, depth + 1, counted)?;
    }
    Ok(())
}

fn bound(field: &str, value: &str, minimum: usize, maximum: usize) -> Result<(), String> {
    let length = value.chars().count();
    if length < minimum {
        return Err(format!("{field} must not be empty"));
    }
    if length > maximum {
        return Err(format!(
            "{field} is {length} characters; the limit is {maximum}"
        ));
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\t' | '\r'))
    {
        return Err(format!("{field} contains control characters"));
    }
    Ok(())
}

fn cardinality(field: &str, actual: usize, maximum: usize) -> Result<(), String> {
    if actual > maximum {
        return Err(format!("{field}: {actual} exceeds the limit of {maximum}"));
    }
    Ok(())
}

/// Only `http`/`https`, and no credentials in the authority. The renderer
/// frames these; it must never be handed a `javascript:` or `data:` document.
fn web_url(url: &str) -> Result<(), String> {
    if url.chars().count() > MAX_URL {
        return Err("url is too long".to_string());
    }
    let rest = match url.split_once("://") {
        Some(("http", rest)) | Some(("https", rest)) => rest,
        _ => return Err(format!("url {url:?} must start with http:// or https://")),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        return Err(format!("url {url:?} has no host"));
    }
    if authority.contains('@') {
        return Err("a url with embedded credentials is not accepted".to_string());
    }
    if url.chars().any(char::is_control) || url.contains(char::is_whitespace) {
        return Err("url contains whitespace or control characters".to_string());
    }
    Ok(())
}

/// Serialize the tree for the browser, replacing host-resolved nodes with the
/// data they stand for. Resolution failures become a visible `error` on the
/// node rather than a missing pane: a source excerpt that stopped resolving is
/// information, not a reason to blank the room.
pub fn resolve(node: &Node, workspace: &Workspace) -> JsonValue {
    let mut value = match serde_json::to_value(node) {
        Ok(JsonValue::Object(map)) => map,
        _ => {
            return json!({ "kind": "text", "tone": "bad", "text": "this node could not be serialized" })
        }
    };

    match node {
        Node::Stack { children, .. } | Node::Row { children, .. } | Node::Deck { children, .. } => {
            let resolved: Vec<JsonValue> = children
                .iter()
                .map(|child| resolve(child, workspace))
                .collect();
            value.insert("children".to_string(), JsonValue::Array(resolved));
        }
        Node::Source { path, from, to } => {
            insert_resolution(&mut value, workspace.excerpt(path, *from, *to));
        }
        Node::Options { filter } => {
            insert_resolution(&mut value, workspace.catalog(filter.as_deref()));
        }
        Node::Diagram {
            nodes,
            edges,
            direction,
            ..
        } => {
            insert_resolution(&mut value, place(nodes, edges, direction.as_deref()));
        }
        _ => {}
    }

    JsonValue::Object(value)
}

/// Build the shared spec from the room's typed diagram fields.
///
/// Tone is deliberately not passed through as the spec's `color`: the spec
/// treats colour as an opaque surface-owned string, and the room's answer is a
/// theme token the browser resolves, not anything the layout should see.
fn spec_of(
    nodes: &[DiagramNode],
    edges: &[DiagramEdge],
    direction: Option<&str>,
) -> Result<diagram::Spec, String> {
    let mut request = json!({
        "nodes": nodes
            .iter()
            .map(|node| {
                let mut object = json!({ "id": node.id });
                if let Some(label) = &node.label {
                    object["label"] = json!(label);
                }
                object
            })
            .collect::<Vec<_>>(),
        "edges": edges
            .iter()
            .map(|edge| {
                let mut object = json!({ "from": edge.from, "to": edge.to });
                if let Some(label) = &edge.label {
                    object["label"] = json!(label);
                }
                if let Some(arrow) = edge.arrow {
                    object["arrow"] = json!(arrow);
                }
                object
            })
            .collect::<Vec<_>>(),
    });
    if let Some(direction) = direction {
        request["direction"] = json!(direction);
    }
    diagram::Spec::parse(&request)
}

/// Lay the diagram out and hand the renderer finished geometry.
///
/// Coordinates come out in a top-left pixel space with a `view_box` around
/// them, so the browser's whole job is to emit one `<rect>` per node and one
/// `<line>` per edge. No layout, no fitting, no arrow trigonometry in JS.
fn place(
    nodes: &[DiagramNode],
    edges: &[DiagramEdge],
    direction: Option<&str>,
) -> Result<JsonValue, String> {
    let spec = spec_of(nodes, edges, direction)?;
    let layout = spec.layout().place(DIAGRAM_SCALE, DIAGRAM_PAD, DIAGRAM_PAD);
    let (_, _, right, bottom) = layout.bounds();

    let placed: Vec<JsonValue> = spec
        .nodes
        .iter()
        .zip(&layout.nodes)
        .zip(nodes)
        .map(|((spec_node, box_), authored)| {
            let (left, top, _, _) = box_.rect();
            json!({
                "id": spec_node.id,
                "label": spec_node.label,
                "tone": authored.tone,
                "x": left,
                "y": top,
                "w": box_.w,
                "h": box_.h,
            })
        })
        .collect();

    // Trim each edge to the two box boundaries so an arrowhead lands on the
    // box instead of stabbing its centre or floating short of it.
    let mut wires: Vec<JsonValue> = Vec::with_capacity(spec.edges.len());
    for edge in &spec.edges {
        let (Some(a), Some(b)) = (layout.nodes.get(edge.from), layout.nodes.get(edge.to)) else {
            continue;
        };
        let (x1, y1) = a.edge_toward(b);
        let (x2, y2) = b.edge_toward(a);
        wires.push(json!({
            "from": spec.nodes.get(edge.from).map(|node| node.id.clone()),
            "to": spec.nodes.get(edge.to).map(|node| node.id.clone()),
            "label": edge.label,
            "arrow": edge.arrow,
            "x1": x1, "y1": y1, "x2": x2, "y2": y2,
        }));
    }

    Ok(json!({
        "nodes": placed,
        "edges": wires,
        "text_size": layout.text_size,
        "view_box": [0.0, 0.0, right + DIAGRAM_PAD, bottom + DIAGRAM_PAD],
    }))
}

fn insert_resolution(value: &mut Map<String, JsonValue>, resolution: Result<JsonValue, String>) {
    match resolution {
        Ok(resolved) => {
            value.insert("resolved".to_string(), resolved);
        }
        Err(error) => {
            value.insert("error".to_string(), JsonValue::String(error));
        }
    }
}

/// Whether this tree shows the runnable catalog. The agent's read-back expands
/// the catalog only when the person is actually looking at it, so `read_room`
/// stays short in the common case.
pub fn shows_options(node: &Node) -> bool {
    matches!(node, Node::Options { .. }) || node.children().iter().any(shows_options)
}

/// A one-line description of what a pane holds, for the agent's read-back.
/// Node kinds in tree order, with the first real text as the gist.
pub fn summarize(node: &Node) -> String {
    let mut kinds = Vec::new();
    let mut gist = None;
    walk(node, &mut kinds, &mut gist);
    let shape = kinds.join("+");
    match gist {
        Some(text) => format!("{shape} — “{}”", clip(&text, 90)),
        None => shape,
    }
}

fn walk(node: &Node, kinds: &mut Vec<&'static str>, gist: &mut Option<String>) {
    match node {
        Node::Stack { .. } | Node::Row { .. } => {}
        Node::Source { path, from, to } => {
            kinds.push("source");
            if gist.is_none() {
                *gist = Some(match (from, to) {
                    (Some(from), Some(to)) => format!("{path}:{from}-{to}"),
                    (Some(from), None) => format!("{path}:{from}-"),
                    _ => path.clone(),
                });
            }
        }
        Node::Embed { url, .. } => {
            kinds.push("embed");
            if gist.is_none() {
                *gist = Some(url.clone());
            }
        }
        Node::Diagram { nodes, edges, .. } => {
            kinds.push("diagram");
            if gist.is_none() {
                // Relations, not geometry. "a→b, b→c" is what the picture
                // means; the coordinates are an implementation detail the
                // agent handed to the host precisely so it wouldn't carry
                // them. An edgeless diagram has only its nodes to report.
                let name = |id: &String| {
                    nodes
                        .iter()
                        .find(|node| &node.id == id)
                        .and_then(|node| node.label.clone())
                        .unwrap_or_else(|| id.clone())
                };
                *gist = Some(if edges.is_empty() {
                    nodes
                        .iter()
                        .map(|node| node.label.clone().unwrap_or_else(|| node.id.clone()))
                        .collect::<Vec<_>>()
                        .join(", ")
                } else {
                    edges
                        .iter()
                        .map(|edge| format!("{}→{}", name(&edge.from), name(&edge.to)))
                        .collect::<Vec<_>>()
                        .join(", ")
                });
            }
        }
        other => {
            kinds.push(other.label());
            if gist.is_none() {
                let text = match other {
                    Node::Heading { text, .. }
                    | Node::Text { text, .. }
                    | Node::Badge { text, .. } => Some(text.clone()),
                    Node::Code { text, .. } => Some(text.lines().next().unwrap_or("").to_string()),
                    Node::List { items, .. } => items.first().cloned(),
                    Node::Kv { items } => items
                        .first()
                        .map(|item| format!("{}: {}", item.label, item.value)),
                    Node::Table { columns, .. } => Some(columns.join(" | ")),
                    Node::Button { label, .. } | Node::Link { label, .. } => Some(label.clone()),
                    _ => None,
                };
                if let Some(text) = text {
                    if !text.trim().is_empty() {
                        *gist = Some(text);
                    }
                }
            }
        }
    }
    for child in node.children() {
        walk(child, kinds, gist);
    }
}

pub fn clip(value: &str, maximum: usize) -> String {
    let flattened = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() <= maximum {
        return flattened;
    }
    let mut clipped: String = flattened.chars().take(maximum).collect();
    clipped.push('…');
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: JsonValue) -> Result<Node, String> {
        serde_json::from_value(value).map_err(|error| error.to_string())
    }

    #[test]
    fn accepts_a_mixed_tree() {
        let node = parse(json!({
            "kind": "stack",
            "children": [
                { "kind": "heading", "text": "Turn loop", "level": 2 },
                { "kind": "source", "path": "crates/ag-ui-surface/src/turn_loop.rs", "from": 1, "to": 20 },
                { "kind": "row", "children": [
                    { "kind": "button", "label": "Next", "ask": "show me the ACP adapter" },
                    { "kind": "badge", "text": "live", "tone": "good" }
                ]}
            ]
        }))
        .expect("tree parses");
        validate(&node).expect("tree validates");
        assert!(summarize(&node).starts_with("heading+source+button+badge"));
    }

    #[test]
    fn rejects_a_script_url_in_a_link() {
        let node = parse(json!({
            "kind": "link", "label": "x", "url": "javascript:alert(1)"
        }))
        .expect("node parses");
        let error = validate(&node).expect_err("javascript: url is rejected");
        assert!(error.contains("http://"), "{error}");
    }

    #[test]
    fn rejects_a_remote_image() {
        let node = parse(json!({ "kind": "image", "src": "https://example.com/a.png" }))
            .expect("node parses");
        validate(&node).expect_err("remote image src is rejected");
    }

    #[test]
    fn rejects_a_traversing_image_path() {
        let node =
            parse(json!({ "kind": "image", "src": "/../../etc/passwd" })).expect("node parses");
        validate(&node).expect_err("traversal in an image src is rejected");
    }

    #[test]
    fn diagram_resolves_to_geometry_the_renderer_can_transcribe() {
        let node = parse(json!({
            "kind": "diagram",
            "nodes": [
                { "id": "a", "label": "input x" },
                { "id": "b", "label": "MatMul", "tone": "accent" },
                { "id": "c", "label": "output y" }
            ],
            "edges": [{ "from": "a", "to": "b" }, { "from": "b", "to": "c" }]
        }))
        .expect("node parses");
        validate(&node).expect("a well-formed diagram validates");

        let resolved = resolve(
            &node,
            &Workspace::open(std::path::Path::new(".")).expect("workspace opens"),
        );
        let placed = &resolved["resolved"];
        assert_eq!(placed["nodes"].as_array().expect("nodes").len(), 3);
        assert_eq!(placed["edges"].as_array().expect("edges").len(), 2);

        // The authored tone survives layout — it is what themes the box.
        assert_eq!(placed["nodes"][1]["tone"], "accent");

        // Everything is inside the view box, and the view box is not inverted;
        // either failure renders as a blank or clipped pane.
        let view_box = placed["view_box"].as_array().expect("view box");
        let (width, height) = (
            view_box[2].as_f64().expect("width"),
            view_box[3].as_f64().expect("height"),
        );
        assert!(width > 0.0 && height > 0.0, "inverted view box");
        for box_ in placed["nodes"].as_array().expect("nodes") {
            let (x, y, w, h) = (
                box_["x"].as_f64().expect("x"),
                box_["y"].as_f64().expect("y"),
                box_["w"].as_f64().expect("w"),
                box_["h"].as_f64().expect("h"),
            );
            assert!(x >= 0.0 && y >= 0.0, "node placed outside the view box");
            assert!(x + w <= width + 1e-6 && y + h <= height + 1e-6);
            assert!(
                w > h,
                "a one-line label should get a wide box, not a square"
            );
        }
    }

    #[test]
    fn diagram_edges_stop_at_the_box_boundary() {
        // An edge that ran to the box centre would put the arrowhead under the
        // node instead of touching it.
        let node = parse(json!({
            "kind": "diagram",
            "nodes": [{ "id": "a" }, { "id": "b" }],
            "edges": [{ "from": "a", "to": "b" }]
        }))
        .expect("node parses");
        let resolved = resolve(
            &node,
            &Workspace::open(std::path::Path::new(".")).expect("workspace opens"),
        );
        let edge = &resolved["resolved"]["edges"][0];
        let boxes = resolved["resolved"]["nodes"].as_array().expect("nodes");

        // Each endpoint must sit exactly ON its box's outline: inside means
        // the arrowhead is buried, outside means it floats short of the box.
        for (index, (x_key, y_key)) in [(0, ("x1", "y1")), (1, ("x2", "y2"))] {
            let b = &boxes[index];
            let (left, top) = (b["x"].as_f64().expect("x"), b["y"].as_f64().expect("y"));
            let (right, bottom) = (
                left + b["w"].as_f64().expect("w"),
                top + b["h"].as_f64().expect("h"),
            );
            let (x, y) = (
                edge[x_key].as_f64().expect("x"),
                edge[y_key].as_f64().expect("y"),
            );
            assert!(
                x >= left - 1e-6 && x <= right + 1e-6 && y >= top - 1e-6 && y <= bottom + 1e-6,
                "endpoint ({x}, {y}) is not on box [{left},{top},{right},{bottom}]"
            );
            let on_outline = [
                (x - left).abs(),
                (x - right).abs(),
                (y - top).abs(),
                (y - bottom).abs(),
            ]
            .iter()
            .any(|gap| *gap < 1e-6);
            assert!(
                on_outline,
                "endpoint ({x}, {y}) is inside the box, not on its edge"
            );
        }
    }

    #[test]
    fn a_diagram_reads_back_as_relations_not_coordinates() {
        let node = parse(json!({
            "kind": "diagram",
            "nodes": [{ "id": "a", "label": "input x" }, { "id": "b", "label": "MatMul" }],
            "edges": [{ "from": "a", "to": "b" }]
        }))
        .expect("node parses");
        let summary = summarize(&node);
        assert!(
            summary.contains("input x→MatMul"),
            "read-back was: {summary}"
        );
        for coordinate in ["x:", "y:", "px"] {
            assert!(
                !summary.contains(coordinate),
                "read-back leaked geometry: {summary}"
            );
        }
    }

    #[test]
    fn a_diagram_with_a_broken_edge_is_rejected_at_validation() {
        // The shared spec owns this judgement, so the room refuses exactly what
        // the canvas refuses rather than rendering half a picture.
        let node = parse(json!({
            "kind": "diagram",
            "nodes": [{ "id": "a" }],
            "edges": [{ "from": "a", "to": "ghost" }]
        }))
        .expect("node parses");
        let error = validate(&node).expect_err("an unknown endpoint does not validate");
        assert!(error.contains("ghost"), "unhelpful error: {error}");
    }

    #[test]
    fn an_empty_diagram_is_rejected() {
        let node = parse(json!({ "kind": "diagram", "nodes": [] })).expect("node parses");
        validate(&node).expect_err("a diagram with no nodes does not validate");
    }

    #[test]
    fn rejects_an_unknown_kind() {
        parse(json!({ "kind": "iframe", "url": "https://example.com" }))
            .expect_err("an unknown node kind does not parse");
    }

    #[test]
    fn rejects_a_ragged_table() {
        let node = parse(json!({
            "kind": "table",
            "columns": ["a", "b"],
            "rows": [["1", "2"], ["3"]]
        }))
        .expect("node parses");
        let error = validate(&node).expect_err("ragged row is rejected");
        assert!(error.contains("row 1"), "{error}");
    }

    #[test]
    fn rejects_an_over_deep_tree() {
        let mut node = Node::Text {
            text: "leaf".to_string(),
            tone: Tone::Neutral,
        };
        for _ in 0..MAX_DEPTH {
            node = Node::Stack {
                children: vec![node],
                gap: None,
            };
        }
        validate(&node).expect_err("a tree past the depth limit is rejected");
    }

    #[test]
    fn rejects_an_empty_container() {
        let node = parse(json!({ "kind": "stack", "children": [] })).expect("node parses");
        validate(&node).expect_err("an empty stack is rejected");
    }

    #[test]
    fn a_deck_flips_and_its_titles_must_match_its_children() {
        let node = parse(json!({
            "kind": "deck",
            "titles": ["one", "two"],
            "children": [
                { "kind": "text", "text": "first" },
                { "kind": "text", "text": "second" }
            ]
        }))
        .expect("node parses");
        validate(&node).expect("a titled deck validates");

        let odd = parse(json!({
            "kind": "deck",
            "titles": ["only one"],
            "children": [
                { "kind": "text", "text": "first" },
                { "kind": "text", "text": "second" }
            ]
        }))
        .expect("node parses");
        let error = validate(&odd).expect_err("mismatched titles are rejected");
        assert!(error.contains("one per child"), "{error}");

        let empty = parse(json!({ "kind": "deck", "children": [] })).expect("node parses");
        validate(&empty).expect_err("an empty deck is rejected");
    }

    #[test]
    fn html_is_bounded_but_not_parsed() {
        let node = parse(json!({
            "kind": "html",
            "html": "<label><input type=checkbox data-point=\"board\"> board</label>",
            "height": 240
        }))
        .expect("node parses");
        validate(&node).expect("sandboxed html validates without being parsed");

        let oversize =
            parse(json!({ "kind": "html", "html": "x".repeat(48_001) })).expect("node parses");
        validate(&oversize).expect_err("oversize html is rejected");

        let flat = parse(json!({ "kind": "html", "html": "<p>hi</p>", "height": 40 }))
            .expect("node parses");
        validate(&flat).expect_err("an unusable height is rejected");
    }

    #[test]
    fn a_source_inside_a_deck_still_resolves() {
        let workspace = Workspace::open(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
            .expect("workspace opens");
        let node = parse(json!({
            "kind": "deck",
            "children": [{ "kind": "source", "path": "Cargo.toml", "from": 1, "to": 2 }]
        }))
        .expect("node parses");
        let resolved = resolve(&node, &workspace);
        let child = &resolved["children"][0];
        assert!(
            child.get("resolved").is_some(),
            "a deck face's source excerpt must be host-resolved like any other: {child}"
        );
    }

    #[test]
    fn rejects_credentials_in_an_embed_url() {
        let node = parse(json!({ "kind": "embed", "url": "https://user:pw@example.com" }))
            .expect("node parses");
        validate(&node).expect_err("embedded credentials are rejected");
    }

    #[test]
    fn accepts_a_data_image() {
        let node = parse(json!({
            "kind": "image",
            "src": "data:image/png;base64,iVBORw0KGgo=",
            "alt": "a capture"
        }))
        .expect("node parses");
        validate(&node).expect("a data:image/ src is accepted");
    }
}
