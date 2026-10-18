use samepage_capability::envelope;
use samepage_trace::{Ring, Trace};
use serde_json::Value;
#[test]
#[ignore = "needs results from the real WASM artifact; run tests/wasm/run-check.sh"]
fn every_export_matches_the_native_capability() {
    let results: Vec<String> = serde_json::from_str(
        &std::fs::read_to_string(std::env::var("SAMEPAGE_TRACE_WASM_RESULTS").unwrap()).unwrap(),
    )
    .unwrap();
    let cases: Value = serde_json::from_str(include_str!("wasm/cases.json")).unwrap();
    let view = cases["view"].to_string();
    let calls = cases["calls"].as_array().unwrap();
    assert_eq!(results.len(), calls.len());
    let mut ring = Ring::default();
    for (index, (c, wasm)) in calls.iter().zip(&results).enumerate() {
        let native = match c["export"].as_str().unwrap() {
            "descriptor" => envelope::descriptor(&Trace),
            "project" => envelope::project(&Trace, &view),
            "digest" => envelope::digest(&Trace, &view),
            "legend" => envelope::legend(&Trace, &view),
            "plan" => envelope::plan(
                &Trace,
                &c["call"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| c["call"].to_string()),
                &view,
            ),
            "validate" => envelope::validate(&Trace, &c["delta"].to_string(), &view),
            "cascade" => envelope::cascade(&Trace, &c["deleted"].to_string(), &view),
            "changes" => envelope::changes(
                &Trace,
                &c["before"].to_string(),
                &c["after"].to_string(),
                "agent",
            ),
            "trace_operation" => samepage_trace::operate(&mut ring, &c["input"].to_string()),
            _ => panic!("unknown export"),
        };
        assert_eq!(
            serde_json::from_str::<Value>(wasm).unwrap(),
            serde_json::from_str::<Value>(&native).unwrap(),
            "case {index}: {c}"
        );
    }
    println!("WASM EQUIVALENCE: {} calls identical", results.len());
}
