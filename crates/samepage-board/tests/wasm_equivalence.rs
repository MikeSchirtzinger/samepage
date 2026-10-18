use samepage_board::BoardCapability;
use samepage_capability::envelope;
use serde_json::Value;
fn raw(v: &Value) -> String {
    v.as_str()
        .map(str::to_string)
        .unwrap_or_else(|| v.to_string())
}
#[test]
#[ignore = "needs actual WASM results; run tests/wasm/run-check.sh"]
fn every_export_matches_the_native_capability() {
    let results: Vec<String> = serde_json::from_str(
        &std::fs::read_to_string(
            std::env::var("SAMEPAGE_BOARD_WASM_RESULTS").expect("real WASM results"),
        )
        .unwrap(),
    )
    .unwrap();
    let cases: Value = serde_json::from_str(include_str!("wasm/cases.json")).unwrap();
    let calls = cases["calls"].as_array().unwrap();
    assert_eq!(results.len(), calls.len());
    for (i, (c, wasm)) in calls.iter().zip(&results).enumerate() {
        let view = c.get("view").unwrap_or(&cases["view"]).to_string();
        let b = raw(&c["board"]);
        let s = c
            .get("since")
            .cloned()
            .unwrap_or(serde_json::json!(0))
            .to_string();
        let native = match c["export"].as_str().unwrap() {
            "descriptor" => envelope::descriptor(&BoardCapability),
            "project" => envelope::project(&BoardCapability, &view),
            "digest" => envelope::digest(&BoardCapability, &view),
            "legend" => envelope::legend(&BoardCapability, &view),
            "plan" => envelope::plan(&BoardCapability, &raw(&c["call"]), &view),
            "validate" => envelope::validate(&BoardCapability, &c["delta"].to_string(), &view),
            "cascade" => envelope::cascade(&BoardCapability, &c["deleted"].to_string(), &view),
            "transition" => samepage_board::transition(&b, &raw(&c["call"]), &raw(&c["context"])),
            "project_board" => samepage_board::project_board(&b, &s),
            "delta" => samepage_board::delta(&b, &s),
            "validate_board" => samepage_board::validate_board(&b),
            "migrate" => samepage_board::migrate(&b),
            "readback" => samepage_board::readback(&b, &s),
            "advisory_preview" => samepage_board::advisory_preview(
                &b,
                c["revision"].as_str().unwrap(),
                c["timestamp"].as_str().unwrap(),
            ),
            "changes" => envelope::changes(
                &BoardCapability,
                &c["before"].to_string(),
                &c["after"].to_string(),
                "agent",
            ),
            other => panic!("unknown export {other}"),
        };
        let actual: Value = serde_json::from_str(wasm).unwrap();
        let expected: Value = serde_json::from_str(&native).unwrap();
        assert_eq!(actual, expected, "case {i}: {c}");
    }
    let values: Vec<Value> = results
        .iter()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert!(values.iter().any(|r| {
        r["ok"]["error"]
            .as_str()
            .is_some_and(|s| s.contains("third") && s.contains("Already written: added a, added b"))
    }));
    assert!(values.iter().any(|r| {
        r["ok"]["entries"]
            .as_array()
            .is_some_and(|es| es.iter().any(|e| e["standing"] == "SETTLED"))
    }));
    for code in [
        "bad_input",
        "unknown_action",
        "human_context_required",
        "native_store_required",
        "board_snapshot_required",
    ] {
        assert!(
            values.iter().any(|v| v["error"]["code"] == code),
            "missing {code}"
        );
    }
    println!("WASM EQUIVALENCE: {} calls identical", results.len());
}
