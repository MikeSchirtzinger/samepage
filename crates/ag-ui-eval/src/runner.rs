use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use thiserror::Error;
use wait_timeout::ChildExt;

use crate::model::{
    ExecutionSpec, Layer, ObservationSource, ProcessReceipt, ProviderSpec, ScenarioReceipt,
    ScenarioSpec, ScenarioStatus, ScorerReceipt, SubjectKind, SubjectReceipt, SuiteReceipt,
    SuiteStatus, SuiteSummary, ThresholdOp, ThresholdSpec, SCHEMA_VERSION,
};

const MAX_CAPTURE_CHARS: usize = 32_768;

#[derive(Debug, Error)]
enum ProcessError {
    #[error("could not start {command:?}: {source}")]
    Spawn {
        command: String,
        source: std::io::Error,
    },
    #[error("could not wait for {command:?}: {source}")]
    Wait {
        command: String,
        source: std::io::Error,
    },
    #[error("could not collect {stream} from {command:?}: {message}")]
    Capture {
        command: String,
        stream: &'static str,
        message: String,
    },
}

struct LoadedScenario {
    source: PathBuf,
    spec: ScenarioSpec,
}

struct ProcessOutput {
    receipt: ProcessReceipt,
    stdout_json: Result<Value, String>,
}

pub fn run_suite(layer: Layer, suite: &Path, project_root: &Path) -> SuiteReceipt {
    let suite_label = suite.display().to_string();
    let scenarios = match load_and_validate_suite(layer, suite) {
        Ok(scenarios) => scenarios,
        Err(errors) => {
            return SuiteReceipt::configuration_error(layer, suite_label, errors);
        }
    };

    let scenario_receipts = scenarios
        .into_iter()
        .map(|loaded| run_scenario(loaded, project_root))
        .collect::<Vec<_>>();
    let passed = scenario_receipts
        .iter()
        .filter(|scenario| scenario.status == ScenarioStatus::Pass)
        .count();
    let failed = scenario_receipts
        .iter()
        .filter(|scenario| scenario.status == ScenarioStatus::Fail)
        .count();
    let skipped = scenario_receipts
        .iter()
        .filter(|scenario| scenario.status == ScenarioStatus::Skipped)
        .count();
    let status = if failed > 0 {
        SuiteStatus::Fail
    } else if skipped == scenario_receipts.len() {
        SuiteStatus::Skipped
    } else if skipped > 0 {
        SuiteStatus::Incomplete
    } else {
        SuiteStatus::Pass
    };

    SuiteReceipt {
        schema_version: SCHEMA_VERSION,
        layer,
        suite: suite_label,
        status,
        summary: SuiteSummary {
            total: scenario_receipts.len(),
            passed,
            failed,
            skipped,
        },
        configuration_errors: Vec::new(),
        scenarios: scenario_receipts,
    }
}

fn load_and_validate_suite(
    expected_layer: Layer,
    suite: &Path,
) -> Result<Vec<LoadedScenario>, Vec<String>> {
    let files = discover_suite_files(suite)?;
    let mut loaded = Vec::with_capacity(files.len());
    let mut errors = Vec::new();

    for path in files {
        match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<ScenarioSpec>(&text) {
                Ok(spec) => {
                    for error in validate_scenario(expected_layer, &spec) {
                        errors.push(format!("{}: {error}", path.display()));
                    }
                    loaded.push(LoadedScenario { source: path, spec });
                }
                Err(error) => errors.push(format!(
                    "{}: invalid scenario JSON: {error}",
                    path.display()
                )),
            },
            Err(error) => errors.push(format!(
                "{}: could not read scenario: {error}",
                path.display()
            )),
        }
    }

    let mut ids = HashSet::new();
    for scenario in &loaded {
        if !ids.insert(scenario.spec.id.as_str()) {
            errors.push(format!(
                "{}: duplicate scenario id {:?}",
                scenario.source.display(),
                scenario.spec.id
            ));
        }
    }

    if errors.is_empty() {
        Ok(loaded)
    } else {
        Err(errors)
    }
}

fn discover_suite_files(suite: &Path) -> Result<Vec<PathBuf>, Vec<String>> {
    if !suite.exists() {
        return Err(vec![format!(
            "suite path {} does not exist; missing suites are configuration errors",
            suite.display()
        )]);
    }

    if suite.is_file() {
        if suite.extension().and_then(|extension| extension.to_str()) != Some("json") {
            return Err(vec![format!(
                "suite file {} is not a .json scenario",
                suite.display()
            )]);
        }
        return Ok(vec![suite.to_path_buf()]);
    }

    if !suite.is_dir() {
        return Err(vec![format!(
            "suite path {} is neither a file nor a directory",
            suite.display()
        )]);
    }

    let entries = match fs::read_dir(suite) {
        Ok(entries) => entries,
        Err(error) => {
            return Err(vec![format!(
                "could not read suite directory {}: {error}",
                suite.display()
            )]);
        }
    };
    let mut files = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                if path.is_file()
                    && path.extension().and_then(|extension| extension.to_str()) == Some("json")
                {
                    files.push(path);
                }
            }
            Err(error) => {
                return Err(vec![format!(
                    "could not enumerate suite directory {}: {error}",
                    suite.display()
                )]);
            }
        }
    }
    files.sort();
    if files.is_empty() {
        return Err(vec![format!(
            "suite directory {} contains no .json scenario files; empty suites are configuration errors",
            suite.display()
        )]);
    }
    Ok(files)
}

fn validate_scenario(expected_layer: Layer, scenario: &ScenarioSpec) -> Vec<String> {
    let mut errors = Vec::new();
    if scenario.schema_version != SCHEMA_VERSION {
        errors.push(format!(
            "schema_version must be {SCHEMA_VERSION}, got {}",
            scenario.schema_version
        ));
    }
    if scenario.layer != expected_layer {
        errors.push(format!(
            "scenario layer {} does not match requested layer {expected_layer}",
            scenario.layer
        ));
    }
    if scenario.layer == Layer::RunScoring {
        errors.push(
            "run_scoring contracts use `validate-run-scoring`; they are never executable scenario suites"
                .to_string(),
        );
    }
    validate_nonempty("scenario id", &scenario.id, &mut errors);
    validate_nonempty("subject id", &scenario.subject.id, &mut errors);
    validate_nonempty(
        "subject capability",
        &scenario.subject.capability,
        &mut errors,
    );

    match scenario.layer {
        Layer::Conformance => {
            if scenario.provider.is_some() {
                errors.push("conformance scenarios must not declare a provider".to_string());
            }
            if !scenario.execution.skip_exit_codes.is_empty()
                || scenario.execution.skip_reason_pointer.is_some()
            {
                errors.push(
                    "conformance scenarios cannot convert execution failures into skips"
                        .to_string(),
                );
            }
        }
        Layer::AgentEval => {
            if scenario.subject.kind != SubjectKind::Extension {
                errors.push("agent_eval scenarios must target an extension subject".to_string());
            }
            match &scenario.provider {
                Some(provider) => validate_provider(provider, &mut errors),
                None => errors.push(
                    "agent_eval scenarios must name the real provider adapter, provider, and model"
                        .to_string(),
                ),
            }
            if !scenario.execution.skip_exit_codes.is_empty() {
                match scenario.execution.skip_reason_pointer.as_deref() {
                    Some(pointer) if pointer.starts_with('/') => {}
                    _ => errors.push(
                        "agent_eval skip_exit_codes require a JSON skip_reason_pointer beginning with '/'"
                            .to_string(),
                    ),
                }
            }
        }
        Layer::RunScoring => {}
    }

    validate_execution(&scenario.execution, &mut errors);
    validate_scorers(&scenario.scorers, &mut errors);
    errors
}

fn validate_nonempty(label: &str, value: &str, errors: &mut Vec<String>) {
    if value.trim().is_empty() {
        errors.push(format!("{label} must not be empty"));
    }
}

fn validate_provider(provider: &ProviderSpec, errors: &mut Vec<String>) {
    validate_nonempty("provider adapter", &provider.adapter, errors);
    validate_nonempty("provider name", &provider.name, errors);
    validate_nonempty("provider model", &provider.model, errors);
}

fn validate_execution(execution: &ExecutionSpec, errors: &mut Vec<String>) {
    validate_nonempty("execution command", &execution.command, errors);
    if execution.timeout_seconds == 0 || execution.timeout_seconds > 3_600 {
        errors.push("execution timeout_seconds must be between 1 and 3600".to_string());
    }
    for command in &execution.required_commands {
        validate_nonempty("required command", command, errors);
    }
    for variable in &execution.required_env {
        validate_nonempty("required environment variable", variable, errors);
    }
}

fn validate_scorers(scorers: &[crate::model::ScorerSpec], errors: &mut Vec<String>) {
    if scorers.is_empty() {
        errors.push("scenario must contain at least one scorer".to_string());
        return;
    }
    let mut ids = HashSet::new();
    let mut has_success_exit = false;
    for scorer in scorers {
        validate_nonempty("scorer id", &scorer.id, errors);
        if !ids.insert(scorer.id.as_str()) {
            errors.push(format!("duplicate scorer id {:?}", scorer.id));
        }
        if let ObservationSource::StdoutJson { pointer } = &scorer.source {
            if !pointer.starts_with('/') {
                errors.push(format!(
                    "scorer {:?} stdout_json pointer must begin with '/'",
                    scorer.id
                ));
            }
        }
        if matches!(scorer.source, ObservationSource::ExitCode)
            && matches!(scorer.threshold.op, ThresholdOp::Eq)
            && scorer.threshold.value == json!(0)
        {
            has_success_exit = true;
        }
        if let Err(error) = validate_threshold(&scorer.threshold) {
            errors.push(format!("scorer {:?}: {error}", scorer.id));
        }
    }
    if !has_success_exit {
        errors.push(
            "scenario must include an exit_code scorer with threshold {\"op\":\"eq\",\"value\":0}"
                .to_string(),
        );
    }
}

fn validate_threshold(threshold: &ThresholdSpec) -> Result<(), String> {
    match threshold.op {
        ThresholdOp::Eq => Ok(()),
        ThresholdOp::Contains => {
            if threshold.value.is_string() {
                Ok(())
            } else {
                Err("contains threshold value must be a string".to_string())
            }
        }
        ThresholdOp::Gte | ThresholdOp::Lte => {
            if threshold.value.is_number() {
                Ok(())
            } else {
                Err("gte/lte threshold value must be a number".to_string())
            }
        }
    }
}

fn run_scenario(loaded: LoadedScenario, project_root: &Path) -> ScenarioReceipt {
    let spec = loaded.spec;
    let subject = SubjectReceipt {
        kind: match spec.subject.kind {
            SubjectKind::Protocol => "protocol",
            SubjectKind::Extension => "extension",
        }
        .to_string(),
        id: spec.subject.id.clone(),
        capability: spec.subject.capability.clone(),
    };

    let missing = missing_requirements(&spec.execution);
    if !missing.is_empty() {
        let reason = missing.join("; ");
        return if spec.layer == Layer::AgentEval {
            ScenarioReceipt {
                id: spec.id,
                status: ScenarioStatus::Skipped,
                subject,
                provider: spec.provider,
                skip_reason: Some(reason),
                failure_reason: None,
                process: None,
                scorers: Vec::new(),
            }
        } else {
            ScenarioReceipt {
                id: spec.id,
                status: ScenarioStatus::Fail,
                subject,
                provider: spec.provider,
                skip_reason: None,
                failure_reason: Some(reason),
                process: None,
                scorers: Vec::new(),
            }
        };
    }

    let process = match execute(&spec.execution, project_root) {
        Ok(process) => process,
        Err(error) => {
            return ScenarioReceipt {
                id: spec.id,
                status: ScenarioStatus::Fail,
                subject,
                provider: spec.provider,
                skip_reason: None,
                failure_reason: Some(error.to_string()),
                process: None,
                scorers: Vec::new(),
            };
        }
    };

    if let Some(code) = process.receipt.exit_code {
        if spec.execution.skip_exit_codes.contains(&code) {
            let reason = extract_skip_reason(
                &process.stdout_json,
                spec.execution.skip_reason_pointer.as_deref(),
            );
            return match reason {
                Ok(reason) => ScenarioReceipt {
                    id: spec.id,
                    status: ScenarioStatus::Skipped,
                    subject,
                    provider: spec.provider,
                    skip_reason: Some(reason),
                    failure_reason: None,
                    process: Some(process.receipt),
                    scorers: Vec::new(),
                },
                Err(error) => ScenarioReceipt {
                    id: spec.id,
                    status: ScenarioStatus::Fail,
                    subject,
                    provider: spec.provider,
                    skip_reason: None,
                    failure_reason: Some(error),
                    process: Some(process.receipt),
                    scorers: Vec::new(),
                },
            };
        }
    }

    let scorer_receipts = spec
        .scorers
        .iter()
        .map(|scorer| evaluate_scorer(scorer, &process))
        .collect::<Vec<_>>();
    let scorers_passed = scorer_receipts.iter().all(|scorer| scorer.passed);
    let status = if !process.receipt.timed_out && scorers_passed {
        ScenarioStatus::Pass
    } else {
        ScenarioStatus::Fail
    };
    let failure_reason = if process.receipt.timed_out {
        Some(format!(
            "execution exceeded {} seconds",
            spec.execution.timeout_seconds
        ))
    } else if !scorers_passed {
        Some("one or more scorers missed their threshold".to_string())
    } else {
        None
    };

    ScenarioReceipt {
        id: spec.id,
        status,
        subject,
        provider: spec.provider,
        skip_reason: None,
        failure_reason,
        process: Some(process.receipt),
        scorers: scorer_receipts,
    }
}

fn missing_requirements(execution: &ExecutionSpec) -> Vec<String> {
    let mut missing = Vec::new();
    for command in &execution.required_commands {
        if !command_available(command) {
            missing.push(format!("required command {command:?} is unavailable"));
        }
    }
    for variable in &execution.required_env {
        let present = std::env::var_os(variable)
            .map(|value| !value.is_empty())
            .unwrap_or(false);
        if !present {
            missing.push(format!(
                "required environment variable {variable:?} is unavailable"
            ));
        }
    }
    missing
}

fn command_available(command: &str) -> bool {
    let candidate = Path::new(command);
    if candidate.components().count() > 1 {
        return candidate.is_file();
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|directory| directory.join(command).is_file())
}

fn execute(execution: &ExecutionSpec, project_root: &Path) -> Result<ProcessOutput, ProcessError> {
    let cwd = execution
        .cwd
        .as_deref()
        .map(|cwd| project_root.join(cwd))
        .unwrap_or_else(|| project_root.to_path_buf());
    let mut command = Command::new(&execution.command);
    command
        .args(&execution.args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = command.spawn().map_err(|source| ProcessError::Spawn {
        command: execution.command.clone(),
        source,
    })?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_reader = thread::spawn(move || read_stream(stdout));
    let stderr_reader = thread::spawn(move || read_stream(stderr));

    let wait = child
        .wait_timeout(Duration::from_secs(execution.timeout_seconds))
        .map_err(|source| ProcessError::Wait {
            command: execution.command.clone(),
            source,
        })?;
    let (status, timed_out) = match wait {
        Some(status) => (status, false),
        None => {
            let _ = child.kill();
            let status = child.wait().map_err(|source| ProcessError::Wait {
                command: execution.command.clone(),
                source,
            })?;
            (status, true)
        }
    };

    let stdout_bytes = join_reader(stdout_reader, &execution.command, "stdout")?;
    let stderr_bytes = join_reader(stderr_reader, &execution.command, "stderr")?;
    let stdout_full = String::from_utf8_lossy(&stdout_bytes);
    let stdout_json =
        serde_json::from_str(&stdout_full).map_err(|error| format!("stdout is not JSON: {error}"));
    let stdout = truncate_capture(&stdout_full);
    let stderr = truncate_capture(&String::from_utf8_lossy(&stderr_bytes));

    Ok(ProcessOutput {
        receipt: ProcessReceipt {
            command: execution.command.clone(),
            exit_code: status.code(),
            timed_out,
            duration_ms: started.elapsed().as_millis(),
            stdout,
            stderr,
        },
        stdout_json,
    })
}

fn read_stream(stream: Option<impl Read>) -> Result<Vec<u8>, std::io::Error> {
    let mut bytes = Vec::new();
    if let Some(mut stream) = stream {
        stream.read_to_end(&mut bytes)?;
    }
    Ok(bytes)
}

fn join_reader(
    reader: thread::JoinHandle<Result<Vec<u8>, std::io::Error>>,
    command: &str,
    stream: &'static str,
) -> Result<Vec<u8>, ProcessError> {
    match reader.join() {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(error)) => Err(ProcessError::Capture {
            command: command.to_string(),
            stream,
            message: error.to_string(),
        }),
        Err(_) => Err(ProcessError::Capture {
            command: command.to_string(),
            stream,
            message: "reader thread panicked".to_string(),
        }),
    }
}

fn truncate_capture(value: &str) -> String {
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(MAX_CAPTURE_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}\n[output truncated by ag-ui-eval]")
    } else {
        prefix
    }
}

fn extract_skip_reason(
    stdout_json: &Result<Value, String>,
    pointer: Option<&str>,
) -> Result<String, String> {
    let pointer = pointer.ok_or_else(|| {
        "skip exit was configured without a skip_reason_pointer; skip cannot be reasonless"
            .to_string()
    })?;
    let value = stdout_json
        .as_ref()
        .map_err(|error| format!("skip exit did not emit valid JSON: {error}"))?;
    let reason = value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .ok_or_else(|| {
            format!("skip exit did not emit a nonempty string at JSON pointer {pointer:?}")
        })?;
    Ok(reason.to_string())
}

fn evaluate_scorer(scorer: &crate::model::ScorerSpec, process: &ProcessOutput) -> ScorerReceipt {
    let observation = match &scorer.source {
        ObservationSource::ExitCode => match process.receipt.exit_code {
            Some(code) => Ok(json!(code)),
            None => Err("process ended without an exit code".to_string()),
        },
        ObservationSource::StdoutJson { pointer } => process
            .stdout_json
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|value| {
                value
                    .pointer(pointer)
                    .cloned()
                    .ok_or_else(|| format!("stdout JSON has no value at pointer {pointer:?}"))
            }),
    };

    match observation {
        Ok(observed) => match threshold_passes(&observed, &scorer.threshold) {
            Ok(passed) => ScorerReceipt {
                id: scorer.id.clone(),
                passed,
                observed: Some(observed),
                threshold: scorer.threshold.clone(),
                error: None,
            },
            Err(error) => ScorerReceipt {
                id: scorer.id.clone(),
                passed: false,
                observed: Some(observed),
                threshold: scorer.threshold.clone(),
                error: Some(error),
            },
        },
        Err(error) => ScorerReceipt {
            id: scorer.id.clone(),
            passed: false,
            observed: None,
            threshold: scorer.threshold.clone(),
            error: Some(error),
        },
    }
}

pub(crate) fn threshold_passes(
    observed: &Value,
    threshold: &ThresholdSpec,
) -> Result<bool, String> {
    match threshold.op {
        ThresholdOp::Eq => Ok(observed == &threshold.value),
        ThresholdOp::Contains => {
            let observed = observed
                .as_str()
                .ok_or_else(|| "contains scorer observed a non-string value".to_string())?;
            let expected = threshold
                .value
                .as_str()
                .ok_or_else(|| "contains threshold is not a string".to_string())?;
            Ok(observed.contains(expected))
        }
        ThresholdOp::Gte | ThresholdOp::Lte => {
            let observed = observed
                .as_f64()
                .ok_or_else(|| "numeric scorer observed a non-number value".to_string())?;
            let expected = threshold
                .value
                .as_f64()
                .ok_or_else(|| "numeric threshold is not a number".to_string())?;
            Ok(match threshold.op {
                ThresholdOp::Gte => observed >= expected,
                ThresholdOp::Lte => observed <= expected,
                ThresholdOp::Eq | ThresholdOp::Contains => false,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_thresholds_are_directional() {
        let gte = ThresholdSpec {
            op: ThresholdOp::Gte,
            value: json!(3),
        };
        let lte = ThresholdSpec {
            op: ThresholdOp::Lte,
            value: json!(3),
        };
        assert_eq!(threshold_passes(&json!(4), &gte), Ok(true));
        assert_eq!(threshold_passes(&json!(4), &lte), Ok(false));
    }

    #[test]
    fn empty_suite_is_a_configuration_error() {
        let directory =
            std::env::temp_dir().join(format!("ag-ui-eval-empty-suite-{}", std::process::id()));
        if directory.exists() {
            fs::remove_dir_all(&directory).expect("remove stale test directory");
        }
        fs::create_dir_all(&directory).expect("create empty test directory");
        let receipt = run_suite(Layer::Conformance, &directory, Path::new("."));
        assert_eq!(receipt.status, SuiteStatus::ConfigurationError);
        fs::remove_dir_all(directory).expect("remove empty test directory");
    }
}
