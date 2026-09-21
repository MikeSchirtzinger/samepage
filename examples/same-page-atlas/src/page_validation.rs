//! Browser observations are ephemeral. Geometry remains owned by core.

use super::{AtlasState, Effect, JsonValue, ToolDef, atlas, json};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

const BROWSER_LEASE: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct BrowserError {
    code: String,
    message: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BrowserReport {
    client_id: String,
    sequence: u64,
    revision: Option<String>,
    errors: Vec<BrowserError>,
}

struct Observation {
    report: BrowserReport,
    received: Instant,
}

#[derive(Default)]
pub(super) struct BrowserRegistry {
    clients: BTreeMap<String, Observation>,
}

impl BrowserRegistry {
    fn record(&mut self, report: BrowserReport) -> Result<(), String> {
        if report.client_id.is_empty()
            || report.client_id.len() > 128
            || report.sequence == 0
            || report.errors.len() > 32
            || report.revision.as_ref().is_some_and(|revision| {
                revision.len() != 64 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            || report.errors.iter().any(|error| {
                error.code.is_empty()
                    || error.code.len() > 80
                    || error.message.is_empty()
                    || error.message.len() > 4096
            })
        {
            return Err("invalid browser validation report".into());
        }
        if let Some(previous) = self.clients.get(&report.client_id) {
            if report.sequence <= previous.report.sequence {
                return Err("out-of-order browser validation report".into());
            }
        } else {
            self.clients
                .retain(|_, observation| observation.received.elapsed() <= BROWSER_LEASE);
            if self.clients.len() >= 32 {
                return Err("browser validation client capacity reached".into());
            }
        }
        self.clients.insert(
            report.client_id.clone(),
            Observation {
                report,
                received: Instant::now(),
            },
        );
        Ok(())
    }

    fn receipt(&self, core: atlas::validation::Validation) -> JsonValue {
        let fresh = self.clients.values().any(|observation| {
            observation.received.elapsed() <= BROWSER_LEASE
                && observation.report.revision.as_ref() == Some(&core.revision)
        });
        // Errors stay latched for every connected browser. Expiry removes
        // that browser's authority to attest to the current page, so expiry
        // alone yields pending, never a pass. A closed tab cannot veto a
        // freshly verified replacement browser forever.
        let errors: Vec<_> = self
            .clients
            .iter()
            .filter(|(_, observation)| observation.received.elapsed() <= BROWSER_LEASE)
            .flat_map(|(id, observation)| {
                observation.report.errors.iter().map(move |error| {
                    json!({
                        "client_id": id, "code": error.code, "message": error.message,
                    })
                })
            })
            .collect();
        let status = if !core.errors.is_empty() || !errors.is_empty() {
            "failed"
        } else if core.unmeasured > 0 || !fresh {
            "pending"
        } else {
            "passed"
        };
        json!({
            "schema_version": 1, "status": status, "revision": core.revision,
            "layout": core,
            "browser": {"current_render": fresh, "errors": errors, "lease_seconds": BROWSER_LEASE.as_secs()},
        })
    }
}

impl AtlasState {
    pub(crate) fn validation_report(&self) -> Result<JsonValue, String> {
        let core = atlas::validation::validate(&self.read()?)?;
        Ok(self.validation.lock().receipt(core))
    }

    pub(crate) fn validation_failure(&self) -> Result<(), String> {
        let report = self.validation_report()?;
        if report["status"] == "failed" {
            return Err(
                json!({"error": "PAGE_VALIDATION_FAILED", "validation": report}).to_string(),
            );
        }
        Ok(())
    }

    pub(super) fn record_validation(&self, body: Option<JsonValue>) -> Result<JsonValue, String> {
        let report: BrowserReport =
            serde_json::from_value(body.ok_or("missing browser report")?)
                .map_err(|error| format!("invalid browser validation report: {error}"))?;
        self.validation.lock().record(report)?;
        // Separate from document authorship. Own edits, late measurements,
        // and runtime errors must all reach a parked model.
        self.changed.notify_waiters();
        self.validation_report()
    }
}

pub(super) fn validate_tool(state: &Arc<AtlasState>) -> ToolDef {
    let state = state.clone();
    ToolDef::new(
        "atlas_validate",
        "Validate the shared page using the host/browser core checks and browser runtime reports. Returns a tool error for overlaps, routing errors, invalid relationships, browser exceptions, or pending browser verification. A pass requires measured geometry and a fresh browser render of this exact document. Errors name affected ids. Validation is observational and never moves cards. After a failed authoring call with operation_applied=true, repair those ids rather than repeat creation. await_input also returns validation failures, including failures caused by your own edits.",
        json!({"type":"object","properties":{},"additionalProperties":false}),
        move |_| {
            let state = state.clone();
            Effect::Query(Box::new(move |_| {
                let report = state.validation_report()?;
                if report["status"] == "passed" {
                    Ok(report.to_string())
                } else {
                    Err(json!({"error": "PAGE_VALIDATION_NOT_PASSED", "validation": report}).to_string())
                }
            }))
        },
    ).agent_only()
}

/// Apply the diagnostic policy to every synchronous Atlas write in one place.
/// A failed post-write check is explicit about the already-applied operation.
pub(super) fn validate_writes(defs: &mut [ToolDef], state: &Arc<AtlasState>) {
    for def in defs {
        let apply = def.apply.clone();
        let state = state.clone();
        def.apply = Arc::new(move |args| {
            let effect = apply(args);
            let Effect::Mutate(run) = effect else {
                return effect;
            };
            let state = state.clone();
            Effect::Mutate(Box::new(move |surface| {
                let before = state
                    .update_sequence
                    .load(std::sync::atomic::Ordering::Acquire);
                let reply = run(surface)?;
                if state
                    .update_sequence
                    .load(std::sync::atomic::Ordering::Acquire)
                    == before
                {
                    return Ok(reply);
                }
                let report = state.validation_report()?;
                if report["status"] == "failed" {
                    Err(json!({
                        "error": "PAGE_VALIDATION_FAILED", "operation_applied": true,
                        "operation_result": reply, "validation": report,
                        "next": "Repair the affected ids. Do not repeat creation. Call atlas_validate after the browser renders.",
                    }).to_string())
                } else {
                    Ok(Some(format!(
                        "{}\nPAGE_VALIDATION {}",
                        reply.unwrap_or_else(|| "Applied.".into()),
                        report
                    )))
                }
            }))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ag_ui_canvas::scene::Scene;

    fn core() -> atlas::validation::Validation {
        atlas::validation::validate(&atlas::read(&Scene::new()).unwrap()).unwrap()
    }

    fn observation(
        sequence: u64,
        revision: Option<String>,
        errors: Vec<BrowserError>,
    ) -> BrowserReport {
        BrowserReport {
            client_id: "browser".into(),
            sequence,
            revision,
            errors,
        }
    }

    #[test]
    fn no_browser_stale_browser_and_unmeasured_geometry_cannot_pass() {
        let mut registry = BrowserRegistry::default();
        assert_eq!(registry.receipt(core())["status"], "pending");
        registry
            .record(observation(1, Some(core().revision), vec![]))
            .unwrap();
        assert_eq!(registry.receipt(core())["status"], "passed");
        let mut unmeasured = core();
        unmeasured.unmeasured = 1;
        assert_eq!(registry.receipt(unmeasured)["status"], "pending");
        registry.clients.get_mut("browser").unwrap().received -=
            BROWSER_LEASE + Duration::from_secs(1);
        assert_eq!(registry.receipt(core())["status"], "pending");
        registry
            .record(observation(2, Some("a".repeat(64)), vec![]))
            .unwrap();
        assert_eq!(registry.receipt(core())["status"], "pending");
    }

    #[test]
    fn late_green_cannot_erase_an_error_and_expiry_cannot_pass() {
        let mut registry = BrowserRegistry::default();
        registry
            .record(observation(
                2,
                None,
                vec![BrowserError {
                    code: "render_error".into(),
                    message: "render failed".into(),
                }],
            ))
            .unwrap();
        assert!(
            registry
                .record(observation(1, Some(core().revision), vec![]))
                .is_err()
        );
        registry.clients.get_mut("browser").unwrap().received -=
            BROWSER_LEASE + Duration::from_secs(1);
        assert_eq!(registry.receipt(core())["status"], "pending");
        registry
            .record(observation(3, Some(core().revision), vec![]))
            .unwrap();
        assert_eq!(registry.receipt(core())["status"], "passed");
    }

    #[test]
    fn closed_tabs_release_capacity_but_an_active_error_still_vetoes_a_pass() {
        let mut registry = BrowserRegistry::default();
        for index in 0..32 {
            let mut report = observation(1, Some(core().revision), vec![]);
            report.client_id = format!("closed-{index}");
            registry.record(report).unwrap();
        }
        for observation in registry.clients.values_mut() {
            observation.received -= BROWSER_LEASE + Duration::from_secs(1);
        }
        registry
            .record(observation(1, Some(core().revision), vec![]))
            .unwrap();
        assert_eq!(registry.clients.len(), 1);
        assert_eq!(registry.receipt(core())["status"], "passed");
        let mut broken = observation(
            1,
            None,
            vec![BrowserError {
                code: "render_error".into(),
                message: "another active tab failed".into(),
            }],
        );
        broken.client_id = "broken".into();
        registry.record(broken).unwrap();
        assert_eq!(registry.receipt(core())["status"], "failed");
    }
}
