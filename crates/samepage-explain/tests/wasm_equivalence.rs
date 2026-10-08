use samepage_capability::envelope;
use samepage_explain::Explain;
use serde_json::Value;
#[test]
#[ignore = "requires real WASM results; run tests/wasm/run-check.sh"]
fn every_export_matches_the_native_capability() {
    let results: Vec<String> = serde_json::from_str(
        &std::fs::read_to_string(std::env::var("SAMEPAGE_EXPLAIN_WASM_RESULTS").unwrap()).unwrap(),
    )
    .unwrap();
    let cases: Value = serde_json::from_str(include_str!("wasm/cases.json")).unwrap();
    let calls = cases["calls"].as_array().unwrap();
    assert_eq!(results.len(), calls.len());
    for (i, (call, wasm)) in calls.iter().zip(&results).enumerate() {
        let view = call.get("view").unwrap_or(&cases["view"]).to_string();
        let native = match call["export"].as_str().unwrap() {
            "descriptor" => envelope::descriptor(&Explain),
            "project" => envelope::project(&Explain, &view),
            "digest" => envelope::digest(&Explain, &view),
            "legend" => envelope::legend(&Explain, &view),
            "plan" => envelope::plan(
                &Explain,
                &match &call["call"] {
                    Value::String(s) => s.clone(),
                    v => v.to_string(),
                },
                &view,
            ),
            "validate" => envelope::validate(&Explain, &call["delta"].to_string(), &view),
            "cascade" => envelope::cascade(&Explain, &call["deleted"].to_string(), &view),
            "changes" => envelope::changes(
                &Explain,
                &call["before"].to_string(),
                &call["after"].to_string(),
                "agent",
            ),
            other => panic!("unknown export {other}"),
        };
        assert_eq!(
            serde_json::from_str::<Value>(wasm).unwrap(),
            serde_json::from_str::<Value>(&native).unwrap(),
            "case {i}: {call}"
        );
    }
    for code in ["invalid", "stale", "unknown_action", "bad_input"] {
        assert!(
            results
                .iter()
                .any(|raw| serde_json::from_str::<Value>(raw).unwrap()["error"]["code"] == code),
            "no {code} case"
        );
    }
    println!("WASM EQUIVALENCE: {} calls identical", results.len());
}
