use samepage_capability::{Capability, Composition, Record, View};
use samepage_doc::{host::Host, Actor, Caller};
use samepage_map::{prop_str, rules, Map};
use samepage_review::Review;
use samepage_sketch::{drawing, parse_points, Sketch};
use serde_json::{json, Value};
use std::sync::Arc;
#[allow(dead_code)]
#[path = "../../../apps/samepage/src/diagram_sources.rs"]
mod diagram_sources;
#[allow(dead_code)]
#[path = "../../../apps/samepage/src/host/review.rs"]
mod review;
#[allow(dead_code)]
#[path = "../../../apps/samepage/src/host/sketch.rs"]
mod sketch_host;
fn human() -> Actor {
    Actor::new("person", Caller::Human).labelled("Human")
}
fn agent() -> Actor {
    Actor::new("worker", Caller::Agent).labelled("Sketch agent")
}
fn host() -> Host {
    Host::in_memory(
        Composition::new(vec![
            samepage_map::descriptor(),
            samepage_review::descriptor(),
            samepage_sketch::descriptor(),
        ])
        .unwrap(),
        vec![Arc::new(Map), Arc::new(Review), Arc::new(Sketch)],
    )
    .unwrap()
    .with_composer("sketch", Arc::new(sketch_host::SketchHost { review: true }))
    .unwrap()
    .with_plan_context("map", &["shape"])
    .unwrap()
    .with_plan_context("review", &["shape"])
    .unwrap()
}
fn invoke(h: &Host, name: &str, args: Value) -> samepage_doc::host::Invoked {
    h.invoke(&human(), name, args).unwrap()
}
fn draw(h: &Host, patch: Value) -> String {
    {
        let existing = patch["id"].as_str().map(str::to_string);
        let result = invoke(h, "sketch", json!({"shapes":[patch]}));
        existing.unwrap_or_else(|| result.ids["shape-0"].clone())
    }
}
fn rect(h: &Host, label: &str, x: f64, y: f64, w: f64, ht: f64) -> String {
    draw(
        h,
        json!({"form":"rect","label":label,"x":x,"y":y,"w":w,"h":ht}),
    )
}
fn text(h: &Host, label: &str, x: f64, y: f64) -> String {
    draw(h, json!({"form":"text","label":label,"x":x,"y":y}))
}
fn card(h: &Host, label: &str, x: f64, y: f64) -> String {
    invoke(
        h,
        "card_place",
        json!({"label":label,"x":x,"y":y,"w":200.0,"h_pref":100.0}),
    )
    .ids["card"]
        .clone()
}
fn stroke(h: &Host, points: &[(f64, f64)]) -> String {
    draw(
        h,
        json!({"form":"ink","points":samepage_sketch::format_points(points)}),
    )
}
fn readings(h: &Host, id: &str) -> samepage_sketch::model::Reading {
    samepage_sketch::model::readings(&drawing(&h.view()))
        .into_iter()
        .find(|r| r.shape == id)
        .unwrap()
}
fn ring(cx: f64, cy: f64, rx: f64, ry: f64) -> Vec<(f64, f64)> {
    (0..=48)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / 48.0;
            (cx + a.cos() * rx, cy + a.sin() * ry)
        })
        .collect()
}
fn lift(h: &Host, id: &str) -> String {
    invoke(h, "shape_lift", json!({"id":id})).ids["lifted-card"].clone()
}
fn export(h: &Host) -> String {
    invoke(h, "sketch_export", json!({})).readback[0].clone()
}
fn import(h: &Host, document: &str) -> samepage_doc::host::Invoked {
    invoke(h, "sketch_import", json!({"document":document}))
}
fn unchanged_refusal(h: &Host, id: &str, expected: &str) {
    let before = (h.revision(), h.encode_full(), h.journal().seq);
    let error = h
        .invoke(&human(), "shape_lift", json!({"id":id}))
        .unwrap_err()
        .to_string();
    assert!(error.contains(expected), "{error}");
    assert_eq!(before, (h.revision(), h.encode_full(), h.journal().seq));
}
#[test]
fn lifting_a_labeled_shape_moves_it_into_the_node_register() {
    let h = host();
    let c = card(&h, "runtime kernel", 900.0, 100.0);
    let s = rect(&h, "cold start path", 100.0, 100.0, 260.0, 120.0);
    let mark = invoke(
        &h,
        "mark_add",
        json!({"target":s,"glyph":"?","text":"is this real?"}),
    )
    .ids["mark"]
        .clone();
    let arrow = draw(&h, json!({"form":"arrow","from":s,"to":c}));
    let constraint = invoke(
        &h,
        "constraint_add",
        json!({"op":"labels","members":[s],"text":"measured, not guessed"}),
    )
    .ids["constraint"]
        .clone();
    let seq = h.journal().seq;
    let lifted = lift(&h, &s);
    let v = h.view();
    let node = v.get(&lifted).unwrap();
    assert_eq!(prop_str(node, "label"), Some("cold start path"));
    assert_eq!(samepage_map::prop_num(node, "x"), Some(100.0));
    assert_eq!(samepage_map::prop_num(node, "y"), Some(100.0));
    assert_eq!(samepage_map::prop_num(node, "w"), Some(260.0));
    assert_eq!(samepage_map::prop_num(node, "h_pref"), Some(120.0));
    assert_eq!(prop_str(node, "created_by"), Some("Human"));
    assert!(!v.contains(&s));
    let m = v.get(&mark).unwrap();
    assert_eq!(prop_str(m, "target"), Some(lifted.as_str()));
    assert_eq!(prop_str(m, "created_by"), Some("Human"));
    assert_eq!(prop_str(m, "text"), Some("is this real?"));
    assert_eq!(
        prop_str(v.get(&arrow).unwrap(), "from"),
        Some(lifted.as_str())
    );
    assert_eq!(rules::membership(&v, &constraint), vec![lifted]);
    assert_eq!(h.journal().seq, seq + 1);
    assert_eq!(
        h.journal().recent.back().unwrap().action.as_deref(),
        Some("shape_lift")
    );
}
#[test]
fn lifting_a_region_consumes_the_single_text_inside_as_its_title() {
    let h = host();
    let s = rect(&h, "", 100.0, 100.0, 400.0, 200.0);
    let t = text(&h, "manifest cache", 150.0, 150.0);
    let n = lift(&h, &s);
    let v = h.view();
    assert_eq!(
        prop_str(v.get(&n).unwrap(), "label"),
        Some("manifest cache")
    );
    assert!(!v.contains(&s) && !v.contains(&t));
}
#[test]
fn lifting_a_region_with_two_texts_refuses_the_ambiguity() {
    let h = host();
    let s = rect(&h, "", 100.0, 100.0, 400.0, 200.0);
    text(&h, "one", 150.0, 150.0);
    text(&h, "two", 150.0, 220.0);
    unchanged_refusal(&h, &s, "2 texts");
}
#[test]
fn lifting_a_connector_or_wordless_shape_is_refused() {
    let h = host();
    let s = draw(
        &h,
        json!({"form":"arrow","x":100,"y":100,"points":"0,0 100,0"}),
    );
    unchanged_refusal(&h, &s, "relates things");
    let ink = stroke(&h, &[(0.0, 0.0), (40.0, 30.0), (90.0, 5.0)]);
    unchanged_refusal(&h, &ink, "no words");
}
#[test]
fn an_open_squiggle_encircles_nothing() {
    let h = host();
    card(&h, "tools/list", 100.0, 100.0);
    let arc: Vec<_> = (0..40)
        .map(|i| {
            let a = std::f64::consts::PI * 1.05 * i as f64 / 39.0;
            (200.0 + a.cos() * 400.0, 150.0 + a.sin() * 400.0)
        })
        .collect();
    let id = stroke(&h, &arc);
    assert_ne!(readings(&h, &id).relation, "encloses");
}
#[test]
fn a_circle_around_sketches_reads_as_enclosing_the_ink() {
    let h = host();
    let s = rect(&h, "", 150.0, 150.0, 120.0, 80.0);
    let t = text(&h, "the hinge", 160.0, 260.0);
    let far = rect(&h, "", 1400.0, 100.0, 120.0, 80.0);
    let loop_id = stroke(&h, &ring(210.0, 220.0, 260.0, 260.0));
    let r = readings(&h, &loop_id);
    assert_eq!(r.relation, "encloses");
    assert!(r.targets.contains(&s) && r.targets.contains(&t) && !r.targets.contains(&far));
    let read = invoke(&h, "sketch_read", json!({})).readback.join("\n");
    assert!(read.contains("ENCLOSES") && read.contains("the hinge"));
    assert!(!read.contains("260.0") && !read.contains("points"));
}
#[test]
fn a_caption_near_a_card_and_a_sketch_reads_the_card_only() {
    let h = host();
    let c = card(&h, "runtime kernel", 100.0, 100.0);
    rect(&h, "", 340.0, 90.0, 200.0, 100.0);
    let t = text(&h, "check this", 310.0, 120.0);
    assert_eq!(readings(&h, &t).targets, vec![c]);
}
#[test]
fn a_bound_arrow_follows_the_node_the_human_drags() {
    let h = host();
    let a = card(&h, "left", 100.0, 100.0);
    let b = card(&h, "right", 700.0, 100.0);
    let arrow = draw(&h, json!({"form":"arrow","from":a,"to":b}));
    let d = drawing(&h.view());
    let before = d.shape_endpoints(d.shape(&arrow).unwrap()).unwrap();
    invoke(&h, "card_move", json!({"id":b,"x":700.0,"y":600.0}));
    let d = drawing(&h.view());
    let after = d.shape_endpoints(d.shape(&arrow).unwrap()).unwrap();
    assert_ne!(before.1, after.1);
    let (l, t, r, b) = d.node(&b).unwrap().bounds();
    assert!(
        after.1 .0 >= l - 1.0
            && after.1 .0 <= r + 1.0
            && after.1 .1 >= t - 1.0
            && after.1 .1 <= b + 1.0
    );
}
#[test]
fn an_arrow_binds_to_a_box_drawn_earlier_in_the_same_sketch() {
    let h = host();
    let ids=invoke(&h,"sketch",json!({"shapes":[{"form":"rect","label":"initialize","x":100,"y":100,"w":200,"h":80,"ref":"a"},{"form":"rect","label":"mint the seat","x":500,"y":100,"w":200,"h":80,"ref":"b"},{"form":"arrow","from":"a","to":"b"}]})).ids;
    let d = drawing(&h.view());
    let arrow = d.shape(&ids["shape-2"]).unwrap();
    assert_eq!(arrow.from, ids["shape-0"]);
    assert_eq!(arrow.to, ids["shape-1"]);
    let (a, b) = d.shape_endpoints(arrow).unwrap();
    assert!((a.0 - 300.0).abs() < 1.0 && (b.0 - 500.0).abs() < 1.0);
    let framed = invoke(
        &h,
        "sketch",
        json!({"shapes":[{"form":"frame","label":"stage","ref":"f"},{"form":"rect","frame":"f"}]}),
    )
    .ids;
    assert_eq!(
        prop_str(h.view().get(&framed["shape-1"]).unwrap(), "frame"),
        Some(framed["shape-0"].as_str())
    );
}
#[test]
fn every_form_survives_an_excalidraw_round_trip() {
    let h = host();
    for form in samepage_sketch::FORMS {
        let p = match *form {
            "ink" | "line" | "arrow" => {
                json!({"form":form,"points":"0,0 40,30 90,10","ink":"amber"})
            }
            "text" => json!({"form":form,"label":"a caption","ink":"amber"}),
            _ => {
                json!({"form":form,"label":"the group","x":700,"y":400,"w":120,"h":90,"ink":"green"})
            }
        };
        draw(&h, p);
    }
    let document = export(&h);
    let parsed: Value = serde_json::from_str(&document).unwrap();
    assert_eq!(parsed["type"], "excalidraw");
    assert_eq!(parsed["version"], 2);
    let fresh = host();
    let landed = import(&fresh, &document);
    let result: Value = serde_json::from_str(&landed.readback[0]).unwrap();
    assert_eq!(result["skipped"], json!([]));
    let a = drawing(&h.view());
    let b = drawing(&fresh.view());
    let forms = |d: &samepage_sketch::model::Drawing| {
        let mut f: Vec<_> = d.shapes.iter().map(|s| s.form.clone()).collect();
        f.sort();
        f
    };
    assert_eq!(forms(&a), forms(&b));
    let ellipse = b.shapes.iter().find(|s| s.form == "ellipse").unwrap();
    assert_eq!(ellipse.ink, "green");
    assert_eq!(
        (ellipse.x, ellipse.y, ellipse.w, ellipse.h),
        (700.0, 400.0, 120.0, 90.0)
    );
    assert_eq!(
        b.shapes
            .iter()
            .find(|s| s.form == "ink")
            .unwrap()
            .points
            .first(),
        Some(&(0.0, 0.0))
    );
    assert!(b
        .shapes
        .iter()
        .any(|s| s.form == "text" && s.label == "a caption"));
    assert!(b
        .shapes
        .iter()
        .any(|s| s.form == "rect" && s.label == "the group"));
}
#[test]
fn an_import_names_what_it_could_not_bring() {
    let h = host();
    let document=json!({"elements":[{"type":"rectangle","id":"keep","width":100,"height":50,"strokeColor":"#e03131"},{"type":"image","id":"drop"},{"type":"embeddable","id":"drop2"}]}).to_string();
    let result = import(&h, &document);
    let data: Value = serde_json::from_str(&result.readback[0]).unwrap();
    assert_eq!(data["imported"], 1);
    assert_eq!(data["skipped"].as_array().unwrap().len(), 2);
    assert!(data["skipped"].to_string().contains("invisible"));
    assert!(data["skipped"].to_string().contains("drop2"));
    assert_eq!(drawing(&h.view()).shapes[0].ink, "red");
}
fn curved() -> String {
    json!({"elements":[{"type":"rectangle","id":"source","x":20,"y":40,"width":160,"height":90,"strokeColor":"#1971c2"},{"type":"diamond","id":"target","x":520,"y":180,"width":160,"height":110,"strokeColor":"#2f9e44"},{"type":"arrow","id":"flow","x":180,"y":85,"points":[[0,0],[130,-60],[250,170],[340,150]],"startBinding":{"elementId":"source"},"endBinding":{"elementId":"target"}}]}).to_string()
}
#[test]
fn an_import_reconnects_bound_curved_arrows_to_imported_shapes() {
    let h = host();
    let landed = import(&h, &curved());
    let result: Value = serde_json::from_str(&landed.readback[0]).unwrap();
    assert_eq!(result["bound_connectors"], 1);
    assert_eq!(result["notes"], json!([]));
    let d = drawing(&h.view());
    let s = d.shapes.iter().find(|s| s.form == "arrow").unwrap();
    assert!(!s.from.is_empty() && !s.to.is_empty());
    assert_eq!(d.shape_path(s).unwrap().len(), 4);
    let before = d.shape_endpoints(s).unwrap().1;
    draw(&h, json!({"id":s.to,"x":820,"y":300}));
    let d2 = drawing(&h.view());
    assert_ne!(
        before,
        d2.shape_endpoints(d2.shape(&s.id).unwrap()).unwrap().1
    );
    let e: Value = serde_json::from_str(&export(&h)).unwrap();
    let a = e["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == "arrow")
        .unwrap();
    assert_eq!(a["points"].as_array().unwrap().len(), 4);
    assert!(a["startBinding"]["elementId"].is_string());
}
#[test]
fn styles_groups_and_frames_survive_an_excalidraw_round_trip() {
    let h = host();
    let frame = draw(&h, json!({"form":"frame","label":"Before"}));
    let styled = draw(
        &h,
        json!({"form":"rect","label":"draft","stroke_width":4,"stroke_style":"dotted","opacity":40,"roundness":"sharp","font_size":20,"angle":0.5,"groups":"g1","frame":frame}),
    );
    let text = draw(
        &h,
        json!({"form":"text","label":"big words","font_size":36,"groups":"g1"}),
    );
    let arrow = draw(
        &h,
        json!({"form":"arrow","points":"0,0 80,40","stroke_style":"dashed","roundness":"round"}),
    );
    let doc = export(&h);
    let e: Value = serde_json::from_str(&doc).unwrap();
    let find = |id: &str| {
        e["elements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == id)
            .unwrap()
    };
    let s = find(&styled);
    assert_eq!(s["strokeWidth"], 4.0);
    assert_eq!(s["strokeStyle"], "dotted");
    assert_eq!(s["opacity"], 40.0);
    assert!(s["roundness"].is_null());
    assert_eq!(s["angle"], 0.5);
    assert_eq!(s["groupIds"], json!(["g1"]));
    assert_eq!(find(&format!("{styled}-text"))["fontSize"], 20.0);
    assert_eq!(find(&frame)["name"], "Before");
    assert_eq!(find(&text)["fontSize"], 36.0);
    assert_eq!(find(&arrow)["roundness"], json!({"type":2}));
    let fresh = host();
    import(&fresh, &doc);
    let d = drawing(&fresh.view());
    let r = d.shapes.iter().find(|s| s.label == "draft").unwrap();
    assert_eq!(
        (r.stroke_width, r.opacity, r.font_size, r.angle),
        (4.0, 40.0, 20.0, 0.5)
    );
    assert_eq!(
        (r.stroke_style.as_str(), r.roundness.as_str()),
        ("dotted", "sharp")
    );
    assert_eq!(r.groups, vec!["g1"]);
    assert_eq!(
        r.frame,
        d.shapes.iter().find(|s| s.form == "frame").unwrap().id
    );
    assert_eq!(
        d.shapes
            .iter()
            .find(|s| s.form == "text")
            .unwrap()
            .font_size,
        36.0
    );
    let a = d.shapes.iter().find(|s| s.form == "arrow").unwrap();
    assert_eq!(
        (a.stroke_style.as_str(), a.roundness.as_str()),
        ("dashed", "round")
    );
}
#[test]
fn partial_prefix_names_every_skipped_element_and_commits_once() {
    let h = host();
    let seq = h.journal().seq;
    let r=h.invoke(&agent(),"atlas_sketch",json!({"shapes":[{"form":"rect","label":"kept"},{"form":"image","ref":"bad"},{"form":"ellipse","ref":"later"}]})).unwrap();
    assert_eq!(r.ids.len(), 1);
    let data: Value = serde_json::from_str(&r.readback[0]).unwrap();
    assert_eq!(data["skipped"].as_array().unwrap().len(), 2);
    assert!(
        data["skipped"].to_string().contains("bad")
            && data["skipped"].to_string().contains("later")
    );
    assert_eq!(h.journal().seq, seq + 1);
    assert_eq!(r.action, "sketch");
}
#[test]
fn lift_deduplicates_region_and_consumed_text_members_and_keeps_both_marks() {
    let h = host();
    let s = rect(&h, "", 100.0, 100.0, 400.0, 200.0);
    let t = text(&h, "title", 150.0, 150.0);
    let ids = [s.clone(), t.clone()];
    let constraint =
        invoke(&h, "constraint_add", json!({"op":"group","members":ids})).ids["constraint"].clone();
    let marks: Vec<_> = ids
        .iter()
        .map(|id| {
            invoke(
                &h,
                "mark_add",
                json!({"target":id,"glyph":"?","text":"check"}),
            )
            .ids["mark"]
                .clone()
        })
        .collect();
    let n = lift(&h, &s);
    let v = h.view();
    assert_eq!(rules::membership(&v, &constraint), vec![n.clone()]);
    assert!(marks
        .iter()
        .all(|id| prop_str(v.get(id).unwrap(), "target") == Some(n.as_str())));
}
#[test]
fn an_agent_cannot_create_a_human_mark_through_sketch() {
    let h = host();
    assert!(h
        .invoke(
            &agent(),
            "sketch",
            json!({"shapes":[{"form":"rect","created_by":"Human"}]})
        )
        .is_err());
    assert!(h.view().is_empty());
}
#[test]
fn invalid_imports_and_forward_bindings_store_nothing() {
    let h = host();
    for args in [
        json!({"document":"not JSON"}),
        json!({"document":"{\"elements\":[{\"type\":\"image\",\"id\":\"x\"}]}"}),
    ] {
        assert!(h.invoke(&human(), "sketch_import", args).is_err());
        assert!(h.view().is_empty());
    }
    assert!(h.invoke(&human(),"sketch",json!({"shapes":[{"form":"arrow","from":"later","to":"later"},{"form":"rect","ref":"later"}]})).is_err());
    assert!(h.view().is_empty());
}
#[test]
fn the_geometry_rejects_non_finite_and_malformed_points() {
    assert_eq!(parse_points("0,0 2,3").len(), 2);
    let h = host();
    assert!(h
        .invoke(
            &human(),
            "sketch",
            json!({"shapes":[{"form":"ink","points":"0,0 NaN,4"}]})
        )
        .is_err());
    let r = Record::new("s", "shape")
        .with("form", "ink")
        .with("points", "0,0 bad");
    let v = View::new("r", [r]).unwrap();
    assert!(Sketch
        .validate(
            &samepage_capability::Delta {
                created: ["s".into()].into(),
                ..Default::default()
            },
            &v
        )
        .is_err());
}
#[test]
fn omitted_review_refuses_a_marked_lift_without_orphaning_records() {
    let h = host();
    let s = rect(&h, "review me", 100.0, 100.0, 260.0, 120.0);
    invoke(
        &h,
        "mark_add",
        json!({"target":s,"glyph":"?","text":"keep my question"}),
    );
    let batch = Sketch
        .plan_batch(
            &samepage_capability::Call {
                action: "shape_lift".into(),
                args: json!({"id":s}),
            },
            &h.view(),
        )
        .unwrap();
    use samepage_doc::host::BatchComposer;
    let error = sketch_host::SketchHost { review: false }
        .compose(
            &samepage_capability::Call {
                action: "shape_lift".into(),
                args: json!({"id":s}),
            },
            &h.view(),
            batch,
        )
        .unwrap_err();
    assert!(error.message.contains("review"));
    assert!(h.view().contains(&s));
}
#[test]
fn a_scribble_over_one_node_reads_as_crossing_it_out() {
    let h = host();
    let c = card(&h, "ActionAudience", 100.0, 100.0);
    card(&h, "elsewhere", 900.0, 100.0);
    let points: Vec<_> = (0..12)
        .map(|i| {
            (
                110.0 + i as f64 / 11.0 * 180.0,
                110.0 + (i % 2) as f64 * 70.0,
            )
        })
        .collect();
    let id = stroke(&h, &points);
    assert_eq!(readings(&h, &id).relation, "crosses-out");
    assert_eq!(readings(&h, &id).targets, vec![c]);
    assert!(invoke(&h, "sketch_read", json!({}))
        .readback
        .join("")
        .contains("CROSSES-OUT"));
}
#[test]
fn removing_a_bound_card_detaches_the_arrow_at_its_last_painted_position() {
    let h = host();
    let a = card(&h, "a", 100.0, 100.0);
    let b = card(&h, "b", 600.0, 100.0);
    let arrow = draw(&h, json!({"form":"arrow","from":a,"to":b}));
    let d = drawing(&h.view());
    let before = d.shape_path(d.shape(&arrow).unwrap()).unwrap();
    invoke(&h, "object_remove", json!({"id":b}));
    let d = drawing(&h.view());
    let s = d.shape(&arrow).unwrap();
    assert!(s.to.is_empty());
    assert_eq!(d.shape_path(s).unwrap(), before);
}
#[test]
fn legacy_constraint_members_convert_and_deduplicate_in_the_lift() {
    let h = host();
    let s = rect(&h, "", 100.0, 100.0, 400.0, 200.0);
    let t = text(&h, "title", 150.0, 150.0);
    let view = h.view();
    let legacy = Record::new("legacy", "constraint")
        .with("op", "group")
        .with("members", format!("{s},{t}"));
    let v = View::new(view.revision(), view.records().cloned().chain([legacy])).unwrap();
    let plan = Sketch
        .plan_batch(
            &samepage_capability::Call {
                action: "shape_lift".into(),
                args: json!({"id":s}),
            },
            &v,
        )
        .unwrap();
    let map = &plan.entries[0].plan;
    assert_eq!(map.ops.iter().filter(|op|matches!(op,samepage_capability::Op::Create{kind,..}if kind=="constraint-member")).count(),1);
    assert!(map.ops.iter().any(|op|matches!(op,samepage_capability::Op::Set{props,..}if props.get("member_schema")==Some(&samepage_capability::Value::from("records")))));
}
