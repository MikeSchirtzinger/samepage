use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

fn temp_directory(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock must follow Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "ag-ui-eval-cli-{name}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create test directory");
    path
}

fn run(layer: &str, suite: &Path, project_root: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ag-ui-eval"))
        .args([
            "run",
            "--layer",
            layer,
            "--suite",
            suite.to_str().expect("UTF-8 suite path"),
            "--project-root",
            project_root.to_str().expect("UTF-8 project root"),
        ])
        .output()
        .expect("run ag-ui-eval")
}

fn receipt(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("runner stdout must be one JSON receipt")
}

fn write_scenario(directory: &Path, value: Value) {
    let serialized = serde_json::to_vec_pretty(&value).expect("serialize scenario");
    fs::write(directory.join("scenario.json"), serialized).expect("write scenario");
}

#[test]
fn missing_suite_exits_two_with_configuration_receipt() {
    let root = temp_directory("missing");
    let missing = root.join("not-there");
    let output = run("conformance", &missing, &root);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(receipt(&output)["status"], "configuration_error");
    fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn threshold_miss_exits_one() {
    let root = temp_directory("threshold");
    let suite = root.join("suite");
    fs::create_dir_all(&suite).expect("create suite");
    write_scenario(
        &suite,
        json!({
            "schema_version": 1,
            "id": "threshold-miss",
            "layer": "conformance",
            "subject": {
                "kind": "protocol",
                "id": "fixture",
                "capability": "numeric observation reaches its declared floor"
            },
            "execution": {
                "command": "sh",
                "args": ["-c", "printf '%s' '{\"score\":0.4}'"],
                "timeout_seconds": 10,
                "required_commands": ["sh"]
            },
            "scorers": [
                {
                    "id": "process",
                    "source": {"kind": "exit_code"},
                    "threshold": {"op": "eq", "value": 0}
                },
                {
                    "id": "score",
                    "source": {"kind": "stdout_json", "pointer": "/score"},
                    "threshold": {"op": "gte", "value": 0.9}
                }
            ]
        }),
    );
    let output = run("conformance", &suite, &root);
    assert_eq!(output.status.code(), Some(1));
    let receipt = receipt(&output);
    assert_eq!(receipt["status"], "fail");
    assert_eq!(receipt["scenarios"][0]["scorers"][1]["passed"], false);
    fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn reasoned_skip_exits_three_without_becoming_pass() {
    let root = temp_directory("skip");
    let suite = root.join("suite");
    fs::create_dir_all(&suite).expect("create suite");
    write_scenario(
        &suite,
        json!({
            "schema_version": 1,
            "id": "provider-skip",
            "layer": "agent_eval",
            "subject": {
                "kind": "extension",
                "id": "fixture",
                "capability": "real provider produces one observable extension outcome"
            },
            "provider": {
                "adapter": "fixture",
                "name": "fixture",
                "model": "fixture"
            },
            "execution": {
                "command": "sh",
                "args": ["-c", "printf '%s' '{\"skip_reason\":\"provider unavailable in fixture\"}'; exit 75"],
                "timeout_seconds": 10,
                "required_commands": ["sh"],
                "skip_exit_codes": [75],
                "skip_reason_pointer": "/skip_reason"
            },
            "scorers": [
                {
                    "id": "process",
                    "source": {"kind": "exit_code"},
                    "threshold": {"op": "eq", "value": 0}
                }
            ]
        }),
    );
    let output = run("agent_eval", &suite, &root);
    assert_eq!(output.status.code(), Some(3));
    let receipt = receipt(&output);
    assert_eq!(receipt["status"], "skipped");
    assert_eq!(receipt["summary"]["passed"], 0);
    assert_eq!(
        receipt["scenarios"][0]["skip_reason"],
        "provider unavailable in fixture"
    );
    fs::remove_dir_all(root).expect("remove test directory");
}
