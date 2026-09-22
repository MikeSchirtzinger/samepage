//! The wasm artifact answers exactly as the native capability does. The
//! results file comes from `tests/wasm/run-check.sh`.

use samepage_capability::envelope;
use samepage_map::Map;
use serde_json::Value as JsonValue;

#[test]
#[ignore = "needs results from the wasm artifact; run tests/wasm/run-check.sh"]
fn every_export_matches_the_native_capability() {
    let results: Vec<String> = serde_json::from_str(
        &std::fs::read_to_string(
            std::env::var("SAMEPAGE_MAP_WASM_RESULTS").expect("set SAMEPAGE_MAP_WASM_RESULTS"),
        )
        .unwrap(),
    )
    .unwrap();
    let cases: JsonValue = serde_json::from_str(include_str!("wasm/cases.json")).unwrap();
    let view = cases["view"].to_string();
    let calls = cases["calls"].as_array().unwrap();
    assert_eq!(results.len(), calls.len());
    for (index, (call, wasm)) in calls.iter().zip(&results).enumerate() {
        let native = match call["export"].as_str().unwrap() {
            "descriptor" => envelope::descriptor(&Map),
            "project" => envelope::project(&Map, &view),
            "digest" => envelope::digest(&Map, &view),
            "legend" => envelope::legend(&Map, &view),
            "plan" => {
                let text = match &call["call"] {
                    JsonValue::String(raw) => raw.clone(),
                    other => other.to_string(),
                };
                envelope::plan(&Map, &text, &view)
            }
            "validate" => envelope::validate(&Map, &call["delta"].to_string(), &view),
            "cascade" => envelope::cascade(&Map, &call["deleted"].to_string(), &view),
            "changes" => {
                let projected: JsonValue =
                    serde_json::from_str(&envelope::project(&Map, &view)).unwrap();
                envelope::changes(
                    &Map,
                    &call["before"].to_string(),
                    &projected["ok"].to_string(),
                    "agent",
                )
            }
            other => panic!("unknown export {other}"),
        };
        let native: JsonValue = serde_json::from_str(&native).unwrap();
        let wasm: JsonValue = serde_json::from_str(wasm).unwrap();
        assert_eq!(wasm, native, "case {index}: {call}");
    }
    // The cases exercise success and every error path the host relies on.
    let codes: Vec<String> = results
        .iter()
        .filter_map(|r| serde_json::from_str::<JsonValue>(r).ok())
        .filter_map(|r| r["error"]["code"].as_str().map(str::to_string))
        .collect();
    for code in [
        "needs_capability",
        "invalid",
        "not_map_object",
        "dangling",
        "bad_input",
    ] {
        assert!(
            codes.iter().any(|c| c == code),
            "no case produced {code}: {codes:?}"
        );
    }
    println!("WASM EQUIVALENCE: {} calls identical", results.len());
}
