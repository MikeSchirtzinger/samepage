use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Conformance,
    AgentEval,
    RunScoring,
}

impl fmt::Display for Layer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Conformance => "conformance",
            Self::AgentEval => "agent_eval",
            Self::RunScoring => "run_scoring",
        };
        formatter.write_str(value)
    }
}

impl FromStr for Layer {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "conformance" => Ok(Self::Conformance),
            "agent_eval" | "agent-eval" => Ok(Self::AgentEval),
            "run_scoring" | "run-scoring" => Ok(Self::RunScoring),
            _ => Err(format!(
                "unknown layer {value:?}; expected conformance, agent_eval, or run_scoring"
            )),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioSpec {
    pub schema_version: u32,
    pub id: String,
    pub layer: Layer,
    pub subject: SubjectSpec,
    #[serde(default)]
    pub provider: Option<ProviderSpec>,
    pub execution: ExecutionSpec,
    pub scorers: Vec<ScorerSpec>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubjectSpec {
    pub kind: SubjectKind,
    pub id: String,
    pub capability: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SubjectKind {
    Protocol,
    Extension,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSpec {
    pub adapter: String,
    pub name: String,
    pub model: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionSpec {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    pub timeout_seconds: u64,
    #[serde(default)]
    pub required_commands: Vec<String>,
    #[serde(default)]
    pub required_env: Vec<String>,
    #[serde(default)]
    pub skip_exit_codes: Vec<i32>,
    #[serde(default)]
    pub skip_reason_pointer: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScorerSpec {
    pub id: String,
    pub source: ObservationSource,
    pub threshold: ThresholdSpec,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ObservationSource {
    ExitCode,
    StdoutJson { pointer: String },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ThresholdSpec {
    pub op: ThresholdOp,
    pub value: Value,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdOp {
    Eq,
    Contains,
    Gte,
    Lte,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuiteStatus {
    Pass,
    Fail,
    Skipped,
    Incomplete,
    ConfigurationError,
}

impl SuiteStatus {
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Pass => 0,
            Self::Fail => 1,
            Self::ConfigurationError => 2,
            Self::Skipped | Self::Incomplete => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioStatus {
    Pass,
    Fail,
    Skipped,
}

#[derive(Debug, Serialize)]
pub struct SuiteReceipt {
    pub schema_version: u32,
    pub layer: Layer,
    pub suite: String,
    pub status: SuiteStatus,
    pub summary: SuiteSummary,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub configuration_errors: Vec<String>,
    pub scenarios: Vec<ScenarioReceipt>,
}

impl SuiteReceipt {
    pub fn configuration_error(layer: Layer, suite: String, errors: Vec<String>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            layer,
            suite,
            status: SuiteStatus::ConfigurationError,
            summary: SuiteSummary {
                total: 0,
                passed: 0,
                failed: 0,
                skipped: 0,
            },
            configuration_errors: errors,
            scenarios: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SuiteSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
}

#[derive(Debug, Serialize)]
pub struct ScenarioReceipt {
    pub id: String,
    pub status: ScenarioStatus,
    pub subject: SubjectReceipt,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<ProcessReceipt>,
    pub scorers: Vec<ScorerReceipt>,
}

#[derive(Debug, Serialize)]
pub struct SubjectReceipt {
    pub kind: String,
    pub id: String,
    pub capability: String,
}

#[derive(Debug, Serialize)]
pub struct ProcessReceipt {
    pub command: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u128,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Serialize)]
pub struct ScorerReceipt {
    pub id: String,
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed: Option<Value>,
    pub threshold: ThresholdSpec,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunScoringContract {
    pub schema_version: u32,
    pub layer: Layer,
    pub contract_id: String,
    pub sampling: SamplingContract,
    pub sessions: Vec<SampledSession>,
    pub summary: RunScoringSummary,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SamplingContract {
    pub population: String,
    pub method: String,
    pub window_start: String,
    pub window_end: String,
    pub sampler_version: String,
    pub inclusion_rule: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampledSession {
    pub session_id: String,
    pub selected_at: String,
    pub selection_probability: f64,
    pub source_ref: String,
    pub extension_id: String,
    pub capabilities: Vec<String>,
    pub provider: ProviderSpec,
    pub outcome: RunScoringOutcome,
    pub scorers: Vec<RunScorerResult>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunScoringOutcome {
    pub status: RunScoringSessionStatus,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RunScoringSessionStatus {
    Pass,
    Fail,
    Skipped,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunScorerResult {
    pub id: String,
    pub observed: Value,
    pub threshold: ThresholdSpec,
    pub passed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunScoringSummary {
    pub sampled: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractValidationStatus {
    Pass,
    ConfigurationError,
}

impl ContractValidationStatus {
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Pass => 0,
            Self::ConfigurationError => 2,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RunScoringValidationReceipt {
    pub schema_version: u32,
    pub layer: Layer,
    pub operation: &'static str,
    pub contract: String,
    pub status: ContractValidationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract_id: Option<String>,
    pub sampled_sessions_in_example: usize,
    pub sampler_implemented: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}
