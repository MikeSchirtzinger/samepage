//! The wasm artifact answers exactly as the native capability does, over the
//! same cases. The results file comes from `run-check.sh`.

use samepage_capability::envelope;
use samepage_capability_fixture::Pins;
use serde_json::Value as JsonValue;

#[test]
#[ignore = "needs results from the wasm artifact; run run-check.sh"]
fn every_export_matches_the_native_capability() {
    let results: Vec<String> = serde_json::from_str(
        &std::fs::read_to_string(
            std::env::var("SAMEPAGE_FIXTURE_WASM_RESULTS")
                .expect("set SAMEPAGE_FIXTURE_WASM_RESULTS"),
        )
        .unwrap(),
    )
    .unwrap();
    let cases: JsonValue = serde_json::from_str(include_str!("../cases.json")).unwrap();
    let view = cases["view"].to_string();
    let calls = cases["calls"].as_array().unwrap();
    assert_eq!(results.len(), calls.len());
    let mut batches = 0;
    for (index, (call, wasm)) in calls.iter().zip(&results).enumerate() {
        let native = match call["export"].as_str().unwrap() {
            "descriptor" => envelope::descriptor(&Pins),
            "project" => envelope::project(&Pins, &view),
            "digest" => envelope::digest(&Pins, &view),
            "legend" => envelope::legend(&Pins, &view),
            "plan" => {
                let text = match &call["raw"] {
                    JsonValue::String(raw) => raw.clone(),
                    _ => call["call"].to_string(),
                };
                envelope::plan(&Pins, &text, &view)
            }
            "validate" => envelope::validate(&Pins, &call["delta"].to_string(), &view),
            "cascade" => envelope::cascade(&Pins, &call["deleted"].to_string(), &view),
            "changes" => envelope::changes(&Pins, "{}", "{}", "agent"),
            other => panic!("unknown export {other}"),
        };
        let native: JsonValue = serde_json::from_str(&native).unwrap();
        let wasm: JsonValue = serde_json::from_str(wasm).unwrap();
        assert_eq!(wasm, native, "case {index}: {call}");
        if wasm["ok"]["entries"].is_array() {
            batches += 1;
        }
    }
    assert!(
        batches >= 3,
        "the cases must cover planned batches, saw {batches}"
    );
    println!("FIXTURE WASM EQUIVALENCE: {} calls identical", results.len());
}
