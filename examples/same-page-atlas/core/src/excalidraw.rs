//! `.excalidraw` in and out.
//!
//! This file is short on purpose, and that is the whole claim being made. The
//! shape vocabulary in [`crate`] was chosen to be Excalidraw's — the same form
//! names, points relative to the element origin, `startBinding`/`endBinding`
//! for arrows — so moving a drawing across the boundary is a field rename plus
//! the two places the models genuinely differ:
//!
//! * **Colour.** Ours is a closed list of six names ([`INKS`]); Excalidraw's is
//!   any hex. Export writes [`ink_hex`]; import snaps to [`nearest_ink`], which
//!   is lossy in exactly one direction and says so.
//! * **Text.** Excalidraw carries a caption as a *separate* text element bound
//!   to its container through `containerId`. We carry it as the container's
//!   `label`. Export splits, import folds back.
//!
//! What deliberately does not round-trip: a `node` is a claim about a place in
//! a repository — a path, a line range, a status, a tone, an author. Exported
//! it is a rectangle with words in it, because that is all an Excalidraw
//! document can hold. Re-importing that rectangle gives you a rectangle, not
//! the claim back. Import therefore only ever produces shapes, and never
//! silently reconstitutes cards it cannot vouch for.

use serde_json::{json, Map as JsonMap, Value as JsonValue};

use ag_ui_canvas::scene::{Author, Scene};

use crate::{
    ink_hex, nearest_ink, place_shape, Atlas, AtlasError, Shape, ShapePatch, DEFAULT_STROKE_WIDTH,
    MAX_POINTS,
};

/// Our form name to Excalidraw's element type, and back.
///
/// Written out rather than passed through. Four of the seven names are
/// identical, which is the point — but `rect`/`rectangle` and `ink`/`freedraw`
/// are not, and a pass-through export silently produced elements Excalidraw
/// does not know, which is a file that opens empty.
const TYPES: &[(&str, &str)] = &[
    ("rect", "rectangle"),
    ("ellipse", "ellipse"),
    ("diamond", "diamond"),
    ("ink", "freedraw"),
    ("line", "line"),
    ("arrow", "arrow"),
    ("text", "text"),
    ("frame", "frame"),
];

fn element_type(form: &str) -> &'static str {
    TYPES
        .iter()
        .find(|(ours, _)| *ours == form)
        .map(|(_, theirs)| *theirs)
        .unwrap_or("rectangle")
}

fn form_of(element_type: &str) -> Option<&'static str> {
    TYPES
        .iter()
        .find(|(_, theirs)| *theirs == element_type)
        .map(|(ours, _)| *ours)
}

/// Excalidraw's own font id for the hand-drawn face.
const FONT_HAND: i64 = 3;
const FONT_SIZE: f64 = 16.0;
const LINE_HEIGHT: f64 = 1.25;

/// Text colour for the parts of an export that were never drawn — the words on
/// a card, a mark's glyph. Distinct from a shape's ink, which is authored.
const NODE_STROKE: &str = "#1e293b";
const NODE_FILL: &str = "#e2e8f0";

/// Write the whole atlas as an `.excalidraw` document.
///
/// Nodes and links come along as rectangles and bound arrows. They are the
/// lossy half — see the module note — but leaving them out would export a
/// drawing with nothing under it, which is not the same drawing.
pub fn to_excalidraw(atlas: &Atlas) -> String {
    let mut elements: Vec<JsonValue> = Vec::new();
    let mut seed = 1_000u64;
    let mut next_seed = move || {
        // Deterministic, so exporting the same atlas twice produces the same
        // bytes: a diffable file beats a randomly-reseeded one, and Excalidraw
        // only needs these to be distinct.
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        ((seed >> 33) as i64).abs()
    };

    for node in &atlas.nodes {
        let text_id = format!("{}-text", node.id);
        let mut container = element(
            "rectangle",
            &node.id,
            node.x,
            node.y,
            node.w,
            node.h,
            NODE_STROKE,
            next_seed(),
        );
        insert(&mut container, "backgroundColor", json!(NODE_FILL));
        insert(&mut container, "fillStyle", json!("solid"));
        insert(&mut container, "roundness", json!({ "type": 3 }));
        insert(
            &mut container,
            "boundElements",
            json!([{ "id": text_id, "type": "text" }]),
        );
        elements.push(JsonValue::Object(container));

        let mut words = node.label.clone();
        if !node.note.is_empty() {
            words.push('\n');
            words.push_str(&node.note);
        }
        if !node.path.is_empty() {
            words.push('\n');
            words.push_str(&node.source_ref());
        }
        elements.push(JsonValue::Object(text_element(
            &text_id,
            node.x + 8.0,
            node.y + 8.0,
            (node.w - 16.0).max(16.0),
            (node.h - 16.0).max(FONT_SIZE),
            &words,
            NODE_STROKE,
            Some(&node.id),
            next_seed(),
        )));
    }

    // A mark is a human flagging a card. Excalidraw has no such kind, so it
    // lands as the sentence it already reads as, parked under its node.
    for mark in &atlas.marks {
        let Some(node) = atlas.node(&mark.target) else {
            continue;
        };
        let words = if mark.answer.is_empty() {
            format!("{} {}", mark.glyph, mark.text)
        } else {
            format!("{} {}\n→ {}", mark.glyph, mark.text, mark.answer)
        };
        elements.push(JsonValue::Object(text_element(
            &mark.id,
            node.x,
            node.y + node.h + 6.0,
            node.w,
            FONT_SIZE * LINE_HEIGHT,
            &words,
            ink_hex("amber"),
            None,
            next_seed(),
        )));
    }

    for edge in &atlas.edges {
        let (Some(from), Some(to)) = (atlas.node(&edge.from), atlas.node(&edge.to)) else {
            continue;
        };
        let (ax, ay) = (from.x + from.w / 2.0, from.y + from.h / 2.0);
        let (bx, by) = (to.x + to.w / 2.0, to.y + to.h / 2.0);
        let mut arrow = element(
            "arrow",
            &edge.id,
            ax,
            ay,
            bx - ax,
            by - ay,
            NODE_STROKE,
            next_seed(),
        );
        insert(
            &mut arrow,
            "points",
            json!([[0.0, 0.0], [bx - ax, by - ay]]),
        );
        insert(&mut arrow, "startArrowhead", JsonValue::Null);
        insert(&mut arrow, "endArrowhead", json!("arrow"));
        insert(&mut arrow, "startBinding", binding(&edge.from));
        insert(&mut arrow, "endBinding", binding(&edge.to));
        elements.push(JsonValue::Object(arrow));
    }

    for shape in atlas.shapes.iter().filter(|shape| shape.form != "segment") {
        elements.extend(shape_elements(atlas, shape, &mut next_seed));
    }

    let skipped = atlas
        .constraints
        .iter()
        .map(|constraint| {
            format!(
                "constraint {:?} ({}) was skipped; Excalidraw has no constraint element vocabulary",
                constraint.id, constraint.op
            )
        })
        .chain(
            atlas
                .shapes
                .iter()
                .filter(|shape| shape.form == "segment")
                .map(|shape| {
                    format!(
                        "segment {:?} was skipped; Excalidraw cannot preserve a MobileSAM mask generation or Atlas identity binding",
                        shape.id
                    )
                }),
        )
        .collect::<Vec<_>>();

    let document = json!({
        "type": "excalidraw",
        "version": 2,
        "source": "same-page-atlas",
        "elements": elements,
        "appState": { "viewBackgroundColor": "#ffffff", "gridSize": null },
        "files": {},
        "skipped": skipped,
        "notes": Vec::<String>::new(),
    });
    serde_json::to_string_pretty(&document).unwrap_or_else(|_| "{}".to_string())
}

fn shape_elements(
    atlas: &Atlas,
    shape: &Shape,
    next_seed: &mut impl FnMut() -> i64,
) -> Vec<JsonValue> {
    let stroke = ink_hex(&shape.ink);
    let (l, t, r, b) = shape.bounds();
    let mut out = Vec::new();

    match shape.form.as_str() {
        "text" => {
            let mut element = text_element(
                &shape.id,
                shape.x,
                shape.y,
                shape.w,
                shape.h,
                &shape.label,
                stroke,
                None,
                next_seed(),
            );
            apply_style(&mut element, shape);
            out.push(JsonValue::Object(element));
            return out;
        }
        "ink" => {
            let mut element = element(
                "freedraw",
                &shape.id,
                shape.x,
                shape.y,
                r - l,
                b - t,
                stroke,
                next_seed(),
            );
            insert(&mut element, "points", points_json(&shape.points));
            insert(&mut element, "pressures", json!([]));
            insert(&mut element, "simulatePressure", json!(true));
            apply_style(&mut element, shape);
            out.push(JsonValue::Object(element));
            return out;
        }
        "line" | "arrow" => {
            // A bound endpoint has no stored coordinate here either — it is
            // resolved from the node's live box. Resolve it once, on the way
            // out, so the exported arrow lands where it looks like it lands.
            let ends = atlas.shape_endpoints(shape);
            let (start, end) = match ends {
                Some((start, end)) => (start, end),
                None => ((shape.x, shape.y), (shape.x, shape.y)),
            };
            let mut element = element(
                element_type(&shape.form),
                &shape.id,
                start.0,
                start.1,
                end.0 - start.0,
                end.1 - start.1,
                stroke,
                next_seed(),
            );
            let relative: Vec<(f64, f64)> = atlas
                .shape_path(shape)
                .unwrap_or_else(|| vec![start, end])
                .into_iter()
                .map(|(x, y)| (x - start.0, y - start.1))
                .collect();
            insert(&mut element, "points", points_json(&relative));
            insert(&mut element, "startArrowhead", JsonValue::Null);
            insert(
                &mut element,
                "endArrowhead",
                match shape.head.as_str() {
                    "triangle" => json!("arrow"),
                    "dot" => json!("dot"),
                    _ => JsonValue::Null,
                },
            );
            if !shape.from.is_empty() {
                insert(&mut element, "startBinding", binding(&shape.from));
            }
            if !shape.to.is_empty() {
                insert(&mut element, "endBinding", binding(&shape.to));
            }
            out.push(JsonValue::Object(element));
        }
        "frame" => {
            // A frame is its own element kind with a `name`, not a rectangle
            // with a caption, so the label travels as the name and no bound
            // text is split off below.
            let mut element = element(
                "frame",
                &shape.id,
                shape.x,
                shape.y,
                shape.w,
                shape.h,
                stroke,
                next_seed(),
            );
            insert(&mut element, "name", json!(shape.label));
            apply_style(&mut element, shape);
            out.push(JsonValue::Object(element));
            return out;
        }
        _ => {
            let mut element = element(
                element_type(&shape.form),
                &shape.id,
                shape.x,
                shape.y,
                shape.w,
                shape.h,
                stroke,
                next_seed(),
            );
            if shape.fill != "none" {
                insert(&mut element, "backgroundColor", json!(ink_hex(&shape.fill)));
                insert(&mut element, "fillStyle", json!("solid"));
            }
            out.push(JsonValue::Object(element));
        }
    }
    if let Some(JsonValue::Object(first)) = out.first_mut() {
        apply_style(first, shape);
    }

    // A caption travels as its own bound element, which is how Excalidraw
    // stores one. Folded back into `label` on the way in.
    if !shape.label.is_empty() {
        let text_id = format!("{}-text", shape.id);
        if let Some(JsonValue::Object(container)) = out.first_mut() {
            insert(
                container,
                "boundElements",
                json!([{ "id": text_id, "type": "text" }]),
            );
        }
        let mut caption = text_element(
            &text_id,
            l + 8.0,
            t + 8.0,
            (r - l - 16.0).max(16.0),
            FONT_SIZE * LINE_HEIGHT,
            &shape.label,
            stroke,
            Some(&shape.id),
            next_seed(),
        );
        // A caption is part of its container: it fades, groups, and frames
        // with it. It keeps its own stroke width and roundness, which mean
        // nothing on text.
        if shape.font_size > 0.0 {
            insert(&mut caption, "fontSize", json!(round(shape.font_size)));
        }
        insert(&mut caption, "opacity", json!(round(shape.opacity)));
        insert(&mut caption, "angle", json!(round(shape.angle)));
        insert(&mut caption, "groupIds", json!(shape.groups));
        insert(&mut caption, "frameId", frame_json(shape));
        out.push(JsonValue::Object(caption));
    }
    out
}

/// Excalidraw's `roundness` for a shape that asked for round corners: type 3
/// (adaptive radius) on a box, type 2 (proportional) on a connector, which
/// is how its own editor writes them.
fn roundness_json(shape: &Shape) -> JsonValue {
    if shape.roundness != "round" {
        return JsonValue::Null;
    }
    match shape.form.as_str() {
        "line" | "arrow" => json!({ "type": 2 }),
        "rect" | "frame" => json!({ "type": 3 }),
        _ => JsonValue::Null,
    }
}

fn frame_json(shape: &Shape) -> JsonValue {
    if shape.frame.is_empty() {
        JsonValue::Null
    } else {
        json!(shape.frame)
    }
}

/// Everything about a shape's look that is not its form or colour, written
/// over the element's defaults.
fn apply_style(element: &mut JsonMap<String, JsonValue>, shape: &Shape) {
    insert(element, "strokeWidth", json!(round(shape.stroke_width)));
    insert(element, "strokeStyle", json!(shape.stroke_style));
    insert(element, "opacity", json!(round(shape.opacity)));
    insert(element, "angle", json!(round(shape.angle)));
    insert(element, "roundness", roundness_json(shape));
    insert(element, "groupIds", json!(shape.groups));
    insert(element, "frameId", frame_json(shape));
    if shape.form == "text" && shape.font_size > 0.0 {
        insert(element, "fontSize", json!(round(shape.font_size)));
    }
}

/// Everything every element carries, so each caller only writes what differs.
///
/// Eight positional arguments, which is one past where clippy starts asking
/// questions. They are the eight fields Excalidraw requires on every element
/// and there is no grouping of them that is not an invented struct standing
/// between this and the format it is writing.
#[allow(clippy::too_many_arguments)]
fn element(
    kind: &str,
    id: &str,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    stroke: &str,
    seed: i64,
) -> JsonMap<String, JsonValue> {
    let mut map = JsonMap::new();
    for (key, value) in [
        ("type", json!(kind)),
        ("id", json!(id)),
        ("x", json!(round(x))),
        ("y", json!(round(y))),
        ("width", json!(round(width.abs()))),
        ("height", json!(round(height.abs()))),
        ("angle", json!(0)),
        ("strokeColor", json!(stroke)),
        ("backgroundColor", json!("transparent")),
        ("fillStyle", json!("solid")),
        ("strokeWidth", json!(2)),
        ("strokeStyle", json!("solid")),
        ("roughness", json!(1)),
        ("opacity", json!(100)),
        ("groupIds", json!([])),
        ("frameId", JsonValue::Null),
        ("roundness", JsonValue::Null),
        ("seed", json!(seed)),
        ("version", json!(1)),
        ("versionNonce", json!(seed)),
        ("isDeleted", json!(false)),
        ("boundElements", JsonValue::Null),
        ("updated", json!(1)),
        ("link", JsonValue::Null),
        ("locked", json!(false)),
    ] {
        map.insert(key.to_string(), value);
    }
    map
}

#[allow(clippy::too_many_arguments)]
fn text_element(
    id: &str,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    words: &str,
    stroke: &str,
    container: Option<&str>,
    seed: i64,
) -> JsonMap<String, JsonValue> {
    let mut map = element("text", id, x, y, width, height, stroke, seed);
    insert(&mut map, "text", json!(words));
    insert(&mut map, "originalText", json!(words));
    insert(&mut map, "fontSize", json!(FONT_SIZE));
    insert(&mut map, "fontFamily", json!(FONT_HAND));
    insert(&mut map, "textAlign", json!("left"));
    insert(&mut map, "verticalAlign", json!("top"));
    insert(&mut map, "lineHeight", json!(LINE_HEIGHT));
    insert(&mut map, "strokeWidth", json!(1));
    insert(
        &mut map,
        "containerId",
        container.map(|id| json!(id)).unwrap_or(JsonValue::Null),
    );
    map
}

fn binding(element_id: &str) -> JsonValue {
    json!({ "elementId": element_id, "focus": 0.0, "gap": 4.0 })
}

fn points_json(points: &[(f64, f64)]) -> JsonValue {
    JsonValue::Array(
        points
            .iter()
            .map(|(x, y)| json!([round(*x), round(*y)]))
            .collect(),
    )
}

fn insert(map: &mut JsonMap<String, JsonValue>, key: &str, value: JsonValue) {
    map.insert(key.to_string(), value);
}

fn round(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// What an import turned into, and what it could not.
#[derive(Debug, Clone, Default)]
pub struct Import {
    pub shapes: Vec<ShapePatch>,
    /// Original element id parallel to `shapes`, used only to reconnect arrow
    /// bindings after every imported shape has received its local CRDT id.
    pub source_ids: Vec<String>,
    pub bindings: Vec<ImportBinding>,
    /// `(shape index, source frame element id)` for every element that sat
    /// in a frame. Resolved to the frame's local id after landing, the same
    /// way arrow bindings are.
    pub frames: Vec<(usize, String)>,
    /// One line per element that did not come across, and why. Never silent:
    /// a document that half-arrives while the page reports success is exactly
    /// the failure this surface exists to make impossible.
    pub skipped: Vec<String>,
    /// Elements that DID come across but changed on the way, such as a connector
    /// endpoint whose target did not import. Kept apart from `skipped` because calling
    /// a shape that is on the board "not imported" is a report the export
    /// route immediately contradicts.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ImportBinding {
    pub shape_index: usize,
    pub from: Option<String>,
    pub to: Option<String>,
}

/// What actually landed in one replica.
#[derive(Debug, Clone, Default)]
pub struct LandedImport {
    pub ids: Vec<String>,
    pub skipped: Vec<String>,
    pub notes: Vec<String>,
    pub bound_connectors: usize,
}

/// Read an `.excalidraw` document into shape patches.
///
/// Nothing is written here — the caller decides which author lands them, which
/// is what keeps an import the human clicked from arriving as an agent edit.
pub fn from_excalidraw(document: &str) -> Result<Import, AtlasError> {
    let parsed: JsonValue =
        serde_json::from_str(document).map_err(|error| format!("not JSON: {error}"))?;
    let elements = parsed
        .get("elements")
        .and_then(JsonValue::as_array)
        .ok_or("that file has no `elements` array — is it an .excalidraw document?")?;

    // Captions first: a bound text is part of its container, not a shape of
    // its own, so it has to be resolved before the container is built.
    let mut captions: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut captions_style: std::collections::HashMap<String, f64> =
        std::collections::HashMap::new();
    for element in elements {
        let (Some(container), Some(words)) = (
            element.get("containerId").and_then(JsonValue::as_str),
            element.get("text").and_then(JsonValue::as_str),
        ) else {
            continue;
        };
        if !container.is_empty() {
            captions.insert(container.to_string(), words.to_string());
            if let Some(size) = element.get("fontSize").and_then(JsonValue::as_f64) {
                captions_style.insert(container.to_string(), size);
            }
        }
    }

    let mut import = Import::default();
    for element in elements {
        if element
            .get("isDeleted")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let kind = element
            .get("type")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        let id = element.get("id").and_then(JsonValue::as_str).unwrap_or("");
        let number = |key: &str| element.get(key).and_then(JsonValue::as_f64).unwrap_or(0.0);
        let ink = nearest_ink(
            element
                .get("strokeColor")
                .and_then(JsonValue::as_str)
                .unwrap_or("#93a1bd"),
        )
        .to_string();

        // A caption is part of its container here, and already went into it.
        if kind == "text"
            && element
                .get("containerId")
                .and_then(JsonValue::as_str)
                .is_some_and(|value| !value.is_empty())
        {
            continue;
        }
        let Some(form) = form_of(kind) else {
            import.skipped.push(if kind.is_empty() {
                format!("element {id:?} has no type")
            } else {
                format!(
                    "{kind} {id:?} — the atlas has no such form, so it would have arrived as an invisible object"
                )
            });
            continue;
        };

        let mut patch = ShapePatch {
            form: Some(form.to_string()),
            x: Some(number("x")),
            y: Some(number("y")),
            ink: Some(ink),
            ..Default::default()
        };
        read_style(element, form, &mut patch, &captions_style, id);

        match form {
            "frame" => {
                patch.w = Some(number("width").max(8.0));
                patch.h = Some(number("height").max(8.0));
                patch.label = element
                    .get("name")
                    .and_then(JsonValue::as_str)
                    .map(|name| name.trim().to_string())
                    .filter(|name| !name.is_empty());
            }
            "rect" | "ellipse" | "diamond" => {
                patch.w = Some(number("width").max(8.0));
                patch.h = Some(number("height").max(8.0));
                if let Some(background) = element.get("backgroundColor").and_then(JsonValue::as_str)
                {
                    // `transparent` is Excalidraw's "no fill"; ours is `none`.
                    patch.fill = Some(if background == "transparent" || background.is_empty() {
                        "none".to_string()
                    } else {
                        nearest_ink(background).to_string()
                    });
                }
                patch.label = captions.get(id).cloned();
            }
            "text" => {
                let words = element
                    .get("text")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if words.is_empty() {
                    import
                        .skipped
                        .push(format!("text {id:?} has no words in it"));
                    continue;
                }
                patch.w = Some(number("width").max(8.0));
                patch.label = Some(words);
            }
            _ => {
                let Some(points) = element.get("points").and_then(JsonValue::as_array) else {
                    import
                        .skipped
                        .push(format!("{kind} {id:?} has no `points`"));
                    continue;
                };
                let parsed: Vec<(f64, f64)> = points
                    .iter()
                    .filter_map(|pair| {
                        let pair = pair.as_array()?;
                        Some((pair.first()?.as_f64()?, pair.get(1)?.as_f64()?))
                    })
                    .collect();
                if parsed.len() < 2 {
                    import.skipped.push(format!(
                        "{kind} {id:?} has {} usable point(s); a stroke needs two",
                        parsed.len()
                    ));
                    continue;
                }
                if parsed.len() > MAX_POINTS {
                    import.skipped.push(format!(
                        "{kind} {id:?} has {} points; the limit is {MAX_POINTS}",
                        parsed.len()
                    ));
                    continue;
                }
                patch.points = Some(crate::format_points(&parsed));
                if form == "arrow" || form == "line" {
                    patch.head = Some(
                        match element.get("endArrowhead").and_then(JsonValue::as_str) {
                            Some("dot") | Some("circle") => "dot",
                            Some(_) => "triangle",
                            None if form == "arrow" => "triangle",
                            None => "none",
                        }
                        .to_string(),
                    );
                    let binding_id = |key: &str| {
                        element
                            .get(key)
                            .and_then(|binding| binding.get("elementId"))
                            .and_then(JsonValue::as_str)
                            .map(str::to_string)
                    };
                    let binding = ImportBinding {
                        shape_index: import.shapes.len(),
                        from: binding_id("startBinding"),
                        to: binding_id("endBinding"),
                    };
                    if binding.from.is_some() || binding.to.is_some() {
                        import.bindings.push(binding);
                    }
                }
            }
        }
        if let Some(frame) = element.get("frameId").and_then(JsonValue::as_str) {
            if !frame.is_empty() && form != "frame" {
                import.frames.push((import.shapes.len(), frame.to_string()));
            }
        }
        import.source_ids.push(id.to_string());
        import.shapes.push(patch);
    }

    if import.shapes.is_empty() {
        return Err(format!(
            "nothing in that document could land on the atlas ({} element(s) skipped)",
            import.skipped.len()
        ));
    }
    Ok(import)
}

/// Everything about an element's look that is not its type or colour.
///
/// Read leniently: an absent field is left unset so the atlas's own defaults
/// apply, and a value outside our range is clamped by `place_shape` rather
/// than refused here, because a drawing with one odd stroke width should
/// still arrive.
fn read_style(
    element: &JsonValue,
    form: &str,
    patch: &mut ShapePatch,
    caption_sizes: &std::collections::HashMap<String, f64>,
    id: &str,
) {
    if let Some(width) = element.get("strokeWidth").and_then(JsonValue::as_f64) {
        if width > 0.0 && width != DEFAULT_STROKE_WIDTH {
            patch.stroke_width = Some(width);
        }
    }
    if let Some(style) = element.get("strokeStyle").and_then(JsonValue::as_str) {
        if style != "solid" {
            patch.stroke_style = Some(style.to_string());
        }
    }
    if let Some(opacity) = element.get("opacity").and_then(JsonValue::as_f64) {
        if opacity < 100.0 {
            patch.opacity = Some(opacity);
        }
    }
    match element.get("roundness") {
        Some(JsonValue::Object(_)) if form != "rect" && form != "frame" => {
            patch.roundness = Some("round".to_string())
        }
        Some(JsonValue::Null) | None if form == "rect" || form == "frame" => {
            patch.roundness = Some("sharp".to_string())
        }
        _ => {}
    }
    let sized = matches!(form, "rect" | "ellipse" | "diamond" | "text" | "frame");
    if let Some(angle) = element.get("angle").and_then(JsonValue::as_f64) {
        if angle != 0.0 && sized {
            patch.angle = Some(angle);
        }
    }
    let font_size = if form == "text" {
        element.get("fontSize").and_then(JsonValue::as_f64)
    } else {
        caption_sizes.get(id).copied()
    };
    if let Some(size) = font_size {
        if size > 0.0 {
            patch.font_size = Some(size);
        }
    }
    if let Some(groups) = element.get("groupIds").and_then(JsonValue::as_array) {
        let ids: Vec<&str> = groups.iter().filter_map(JsonValue::as_str).collect();
        if !ids.is_empty() {
            patch.groups = Some(ids.join(" "));
        }
    }
}

/// Land a parsed import and reconnect every binding whose target also landed.
/// Shapes are created first and patched second because imported ids belong to
/// another document and cannot be reused as local CRDT object ids.
pub fn land_import(
    scene: &mut Scene,
    import: Import,
    author: &Author,
    dx: f64,
    dy: f64,
) -> LandedImport {
    let mut result = LandedImport {
        skipped: import.skipped,
        notes: import.notes,
        ..LandedImport::default()
    };
    let mut local_ids: Vec<Option<String>> = Vec::with_capacity(import.shapes.len());
    for patch in &import.shapes {
        let mut patch = patch.clone();
        patch.x = Some(patch.x.unwrap_or_default() + dx);
        patch.y = Some(patch.y.unwrap_or_default() + dy);
        patch.from = None;
        patch.to = None;
        match place_shape(scene, &patch, author) {
            Ok(id) => {
                result.ids.push(id.clone());
                local_ids.push(Some(id));
            }
            Err(error) => {
                result.skipped.push(error);
                local_ids.push(None);
            }
        }
    }

    let source_to_local = import
        .source_ids
        .iter()
        .zip(&local_ids)
        .filter_map(|(source, local)| {
            local
                .as_ref()
                .map(|local| (source.as_str(), local.as_str()))
        })
        .collect::<std::collections::HashMap<_, _>>();

    let mut unresolved = 0usize;
    for binding in &import.bindings {
        let Some(Some(connector)) = local_ids.get(binding.shape_index) else {
            continue;
        };
        let mapped = |source: &Option<String>| {
            source
                .as_deref()
                .and_then(|source| source_to_local.get(source).copied())
                .map(str::to_string)
        };
        let from = mapped(&binding.from);
        let to = mapped(&binding.to);
        unresolved += usize::from(binding.from.is_some() && from.is_none());
        unresolved += usize::from(binding.to.is_some() && to.is_none());
        if from.is_none() && to.is_none() {
            continue;
        }
        let patch = ShapePatch {
            id: Some(connector.clone()),
            from,
            to,
            ..ShapePatch::default()
        };
        match place_shape(scene, &patch, author) {
            Ok(_) => result.bound_connectors += 1,
            Err(error) => {
                unresolved +=
                    usize::from(binding.from.is_some()) + usize::from(binding.to.is_some());
                result
                    .notes
                    .push(format!("connector {connector} landed loose: {error}"));
            }
        }
    }
    if unresolved > 0 {
        result.notes.push(format!(
            "{unresolved} connector endpoint(s) landed loose because their target did not import"
        ));
    }

    // Frame membership is an id in another document's namespace, so it is
    // reconnected after landing exactly like a binding. A member whose frame
    // did not import lands free, and says so.
    let mut unframed = 0usize;
    for (index, frame) in &import.frames {
        let Some(Some(member)) = local_ids.get(*index) else {
            continue;
        };
        let Some(local_frame) = source_to_local.get(frame.as_str()) else {
            unframed += 1;
            continue;
        };
        let patch = ShapePatch {
            id: Some(member.clone()),
            frame: Some((*local_frame).to_string()),
            ..ShapePatch::default()
        };
        if let Err(error) = place_shape(scene, &patch, author) {
            unframed += 1;
            result
                .notes
                .push(format!("shape {member} landed outside its frame: {error}"));
        }
    }
    if unframed > 0 {
        result.notes.push(format!(
            "{unframed} shape(s) landed outside their frame because the frame did not import"
        ));
    }
    result
}
