//! Retained Excalidraw field conversion, independent of the old host.
use serde_json::{json, Map as JsonMap, Value as JsonValue};

use crate::model::{Drawing, Shape, DEFAULT_STROKE_WIDTH};
use crate::{ink_hex, nearest_ink, ShapePatch, MAX_POINTS};
type AtlasError = String;
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

/// Export the drawn layer as an `.excalidraw` document.
pub fn to_excalidraw(atlas: &Drawing) -> String {
    let mut elements: Vec<JsonValue> = Vec::new();
    let mut seed = 1_000u64;
    let mut next_seed = move || {
        // Deterministic, so exporting the same atlas twice produces the same
        // bytes: a diffable file beats a randomly-reseeded one, and Excalidraw
        // only needs these to be distinct.
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        ((seed >> 33) as i64).abs()
    };

    for shape in atlas
        .shapes
        .iter()
        .filter(|shape| crate::FORMS.contains(&shape.form.as_str()))
    {
        elements.extend(shape_elements(atlas, shape, &mut next_seed));
    }

    let skipped: Vec<String> = atlas
        .shapes
        .iter()
        .filter(|s| !crate::FORMS.contains(&s.form.as_str()))
        .map(|s| format!("unsupported form {:?} [{}] was skipped", s.form, s.id))
        .collect();
    let document = json!({
        "type": "excalidraw",
        "version": 2,
        "source": "samepage-sketch",
        "elements": elements,
        "appState": { "viewBackgroundColor": "#ffffff", "gridSize": null },
        "files": {},
        "skipped": skipped,
        "notes": Vec::<String>::new(),
    });
    serde_json::to_string_pretty(&document).unwrap_or_else(|_| "{}".to_string())
}

fn shape_elements(
    atlas: &Drawing,
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
                    "{kind} {id:?} — sketch has no such form, so it would have arrived as an invisible object"
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
            "nothing in that document could land in sketch; skipped: {}",
            import.skipped.join("; ")
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
