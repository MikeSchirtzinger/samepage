use serde_json::Value;

#[test]
#[ignore = "needs actual WASM results; run tests/wasm/run-check.sh"]
fn viewer_relative_exports_match_native_without_changing_existing_tests() {
    let results: Vec<String> = serde_json::from_str(
        &std::fs::read_to_string(
            std::env::var("SAMEPAGE_BOARD_READER_WASM_RESULTS").expect("real WASM reader results"),
        )
        .unwrap(),
    )
    .unwrap();
    let cases: Value = serde_json::from_str(include_str!("wasm/cases.json")).unwrap();
    let calls = cases["reader_calls"].as_array().unwrap();
    assert_eq!(results.len(), calls.len());
    for (i, (call, actual)) in calls.iter().zip(&results).enumerate() {
        let board = call["board"].to_string();
        let since = call.get("since").unwrap_or(&Value::from(0)).to_string();
        let reader = call["reader"].to_string();
        let native = match call["export"].as_str().unwrap() {
            "project_board_for" => samepage_board::project_board_for(&board, &since, &reader),
            "readback_for" => samepage_board::readback_for(&board, &since, &reader),
            export => panic!("unknown reader export {export}"),
        };
        let expected: Value = serde_json::from_str(&native).unwrap();
        let actual: Value = serde_json::from_str(actual).unwrap();
        assert_eq!(actual, expected, "reader case {i}: {call}");
    }
    println!("WASM READER EQUIVALENCE: {} calls identical", results.len());
}
