use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::model::{
    ContractValidationStatus, Layer, RunScoringContract, RunScoringSessionStatus,
    RunScoringValidationReceipt, SCHEMA_VERSION,
};
use crate::runner::threshold_passes;

pub fn validate_contract(path: &Path) -> RunScoringValidationReceipt {
    let contract_label = path.display().to_string();
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            return error_receipt(
                contract_label,
                None,
                0,
                vec![format!("could not read run-scoring contract: {error}")],
            );
        }
    };
    let contract = match serde_json::from_str::<RunScoringContract>(&text) {
        Ok(contract) => contract,
        Err(error) => {
            return error_receipt(
                contract_label,
                None,
                0,
                vec![format!("invalid run-scoring contract JSON: {error}")],
            );
        }
    };
    let errors = validate(&contract);
    let status = if errors.is_empty() {
        ContractValidationStatus::Pass
    } else {
        ContractValidationStatus::ConfigurationError
    };

    RunScoringValidationReceipt {
        schema_version: SCHEMA_VERSION,
        layer: Layer::RunScoring,
        operation: "contract_validation",
        contract: contract_label,
        status,
        contract_id: Some(contract.contract_id),
        sampled_sessions_in_example: contract.sessions.len(),
        sampler_implemented: false,
        errors,
    }
}

fn error_receipt(
    contract: String,
    contract_id: Option<String>,
    sampled_sessions_in_example: usize,
    errors: Vec<String>,
) -> RunScoringValidationReceipt {
    RunScoringValidationReceipt {
        schema_version: SCHEMA_VERSION,
        layer: Layer::RunScoring,
        operation: "contract_validation",
        contract,
        status: ContractValidationStatus::ConfigurationError,
        contract_id,
        sampled_sessions_in_example,
        sampler_implemented: false,
        errors,
    }
}

fn validate(contract: &RunScoringContract) -> Vec<String> {
    let mut errors = Vec::new();
    if contract.schema_version != SCHEMA_VERSION {
        errors.push(format!(
            "schema_version must be {SCHEMA_VERSION}, got {}",
            contract.schema_version
        ));
    }
    if contract.layer != Layer::RunScoring {
        errors.push(format!("layer must be run_scoring, got {}", contract.layer));
    }
    require_text("contract_id", &contract.contract_id, &mut errors);
    require_text(
        "sampling.population",
        &contract.sampling.population,
        &mut errors,
    );
    require_text("sampling.method", &contract.sampling.method, &mut errors);
    require_text(
        "sampling.window_start",
        &contract.sampling.window_start,
        &mut errors,
    );
    require_text(
        "sampling.window_end",
        &contract.sampling.window_end,
        &mut errors,
    );
    require_text(
        "sampling.sampler_version",
        &contract.sampling.sampler_version,
        &mut errors,
    );
    require_text(
        "sampling.inclusion_rule",
        &contract.sampling.inclusion_rule,
        &mut errors,
    );
    if contract.sessions.is_empty() {
        errors.push(
            "sessions must contain at least one sampled session in a scored batch".to_string(),
        );
    }

    let mut session_ids = HashSet::new();
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    for (index, session) in contract.sessions.iter().enumerate() {
        let prefix = format!("sessions[{index}]");
        require_text(
            &format!("{prefix}.session_id"),
            &session.session_id,
            &mut errors,
        );
        if !session_ids.insert(session.session_id.as_str()) {
            errors.push(format!("{prefix}.session_id is duplicated"));
        }
        require_text(
            &format!("{prefix}.selected_at"),
            &session.selected_at,
            &mut errors,
        );
        if !session.selection_probability.is_finite()
            || session.selection_probability <= 0.0
            || session.selection_probability > 1.0
        {
            errors.push(format!(
                "{prefix}.selection_probability must be greater than 0 and at most 1"
            ));
        }
        require_text(
            &format!("{prefix}.source_ref"),
            &session.source_ref,
            &mut errors,
        );
        require_text(
            &format!("{prefix}.extension_id"),
            &session.extension_id,
            &mut errors,
        );
        if session.capabilities.is_empty() {
            errors.push(format!("{prefix}.capabilities must not be empty"));
        }
        for capability in &session.capabilities {
            require_text(&format!("{prefix}.capabilities[]"), capability, &mut errors);
        }
        require_text(
            &format!("{prefix}.provider.adapter"),
            &session.provider.adapter,
            &mut errors,
        );
        require_text(
            &format!("{prefix}.provider.name"),
            &session.provider.name,
            &mut errors,
        );
        require_text(
            &format!("{prefix}.provider.model"),
            &session.provider.model,
            &mut errors,
        );

        match session.outcome.status {
            RunScoringSessionStatus::Pass => passed += 1,
            RunScoringSessionStatus::Fail => failed += 1,
            RunScoringSessionStatus::Skipped => {
                skipped += 1;
                let has_reason = session
                    .outcome
                    .reason
                    .as_deref()
                    .map(str::trim)
                    .map(|reason| !reason.is_empty())
                    .unwrap_or(false);
                if !has_reason {
                    errors.push(format!(
                        "{prefix}.outcome.reason is required when status is skipped"
                    ));
                }
            }
        }

        if session.outcome.status != RunScoringSessionStatus::Skipped && session.scorers.is_empty()
        {
            errors.push(format!("{prefix}.scorers must not be empty"));
        }
        let mut scorer_ids = HashSet::new();
        let mut every_scorer_passed = true;
        for (scorer_index, scorer) in session.scorers.iter().enumerate() {
            let scorer_prefix = format!("{prefix}.scorers[{scorer_index}]");
            require_text(&format!("{scorer_prefix}.id"), &scorer.id, &mut errors);
            if !scorer_ids.insert(scorer.id.as_str()) {
                errors.push(format!("{scorer_prefix}.id is duplicated"));
            }
            match threshold_passes(&scorer.observed, &scorer.threshold) {
                Ok(calculated) if calculated == scorer.passed => {}
                Ok(calculated) => errors.push(format!(
                    "{scorer_prefix}.passed is {}, but its observed value and threshold calculate to {calculated}",
                    scorer.passed
                )),
                Err(error) => errors.push(format!("{scorer_prefix}: {error}")),
            }
            every_scorer_passed &= scorer.passed;
        }
        match session.outcome.status {
            RunScoringSessionStatus::Pass if !every_scorer_passed => errors.push(format!(
                "{prefix}.outcome is pass but at least one scorer missed its threshold"
            )),
            RunScoringSessionStatus::Fail if every_scorer_passed => errors.push(format!(
                "{prefix}.outcome is fail but every scorer met its threshold"
            )),
            RunScoringSessionStatus::Pass
            | RunScoringSessionStatus::Fail
            | RunScoringSessionStatus::Skipped => {}
        }
    }

    if contract.summary.sampled != contract.sessions.len() {
        errors.push(format!(
            "summary.sampled is {}, but sessions contains {} entries",
            contract.summary.sampled,
            contract.sessions.len()
        ));
    }
    if contract.summary.passed != passed {
        errors.push(format!(
            "summary.passed is {}, but {passed} sessions have pass status",
            contract.summary.passed
        ));
    }
    if contract.summary.failed != failed {
        errors.push(format!(
            "summary.failed is {}, but {failed} sessions have fail status",
            contract.summary.failed
        ));
    }
    if contract.summary.skipped != skipped {
        errors.push(format!(
            "summary.skipped is {}, but {skipped} sessions have skipped status",
            contract.summary.skipped
        ));
    }
    errors
}

fn require_text(label: &str, value: &str, errors: &mut Vec<String>) {
    if value.trim().is_empty() {
        errors.push(format!("{label} must not be empty"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ProviderSpec, RunScorerResult, RunScoringOutcome, RunScoringSummary, SampledSession,
        SamplingContract, ThresholdOp, ThresholdSpec,
    };
    use serde_json::json;

    #[test]
    fn skipped_session_requires_a_reason() {
        let contract = RunScoringContract {
            schema_version: SCHEMA_VERSION,
            layer: Layer::RunScoring,
            contract_id: "example".to_string(),
            sampling: SamplingContract {
                population: "sessions".to_string(),
                method: "uniform".to_string(),
                window_start: "2026-01-01T00:00:00Z".to_string(),
                window_end: "2026-01-02T00:00:00Z".to_string(),
                sampler_version: "1".to_string(),
                inclusion_rule: "completed sessions".to_string(),
            },
            sessions: vec![SampledSession {
                session_id: "session-1".to_string(),
                selected_at: "2026-01-02T01:00:00Z".to_string(),
                selection_probability: 1.0,
                source_ref: "receipt://session-1".to_string(),
                extension_id: "atlas".to_string(),
                capabilities: vec!["place node".to_string()],
                provider: ProviderSpec {
                    adapter: "adapter".to_string(),
                    name: "provider".to_string(),
                    model: "model".to_string(),
                },
                outcome: RunScoringOutcome {
                    status: RunScoringSessionStatus::Skipped,
                    reason: None,
                },
                scorers: vec![RunScorerResult {
                    id: "s1".to_string(),
                    observed: json!(1),
                    threshold: ThresholdSpec {
                        op: ThresholdOp::Eq,
                        value: json!(1),
                    },
                    passed: true,
                }],
            }],
            summary: RunScoringSummary {
                sampled: 1,
                passed: 0,
                failed: 0,
                skipped: 1,
            },
        };
        assert!(validate(&contract)
            .iter()
            .any(|error| error.contains("reason is required")));
    }
}
