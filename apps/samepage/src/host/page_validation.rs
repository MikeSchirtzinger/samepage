//! Fresh browser measurements and errors are host session evidence. They never
//! enter the shared CRDT or advance a participant's review read cursor.
use samepage_capability::View;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::sync::Notify;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub stream: String,
    pub revision: String,
    pub sequence: u64,
    pub bounds: BTreeMap<String, [f64; 4]>,
    pub errors: Vec<BrowserError>,
}
#[derive(Clone, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserError {
    pub code: String,
    pub message: String,
}
struct Seen {
    observation: Observation,
    at: Instant,
}
#[derive(Default)]
struct State {
    generation: u64,
    peers: BTreeMap<(String, String), Seen>,
}
#[derive(Default)]
pub struct PageValidation {
    state: Mutex<State>,
    changed: Notify,
}
impl PageValidation {
    pub fn observe(
        &self,
        participant: &str,
        input: Observation,
        view: &View,
    ) -> Result<(), String> {
        if input.stream.is_empty()
            || input.stream.len() > 128
            || input.stream.chars().any(char::is_control)
        {
            return Err("invalid browser observation stream".into());
        }
        let peer = (participant.to_string(), input.stream.clone());
        if input.bounds.len() > 2048 || input.errors.len() > 32 {
            return Err("browser validation report exceeds limits".into());
        }
        for (id, b) in &input.bounds {
            if view.get(id).is_none_or(|r| r.kind != "node")
                || b.iter().any(|v| !v.is_finite() || v.abs() > 1_000_000.0)
                || b[2] <= b[0]
                || b[3] <= b[1]
            {
                return Err("invalid browser card bounds".into());
            }
        }
        if input
            .errors
            .iter()
            .any(|e| e.code.len() > 100 || e.message.len() > 2000)
        {
            return Err("browser error report exceeds limits".into());
        }
        if input.revision != view.revision() {
            return Err("browser validation is for a stale revision".into());
        }
        let mut state = self.state.lock().map_err(|_| "validation lock poisoned")?;
        if state
            .peers
            .get(&peer)
            .is_some_and(|p| p.observation.sequence >= input.sequence)
        {
            return Err("stale browser validation sequence".into());
        }
        // Bound session evidence. Stale observations cannot hide a fresh one.
        state
            .peers
            .retain(|_, p| p.at.elapsed() < Duration::from_secs(60));
        if state.peers.len() >= 64 && !state.peers.contains_key(&peer) {
            return Err("too many validation peers".into());
        }
        state.peers.insert(
            peer,
            Seen {
                observation: input,
                at: Instant::now(),
            },
        );
        state.generation += 1;
        drop(state);
        self.changed.notify_waiters();
        Ok(())
    }
    pub fn generation(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .generation
    }
    pub fn report(&self, view: &View) -> Result<Value, String> {
        let mut report =
            samepage_map::validation::report(view, &BTreeMap::new()).map_err(|e| e.to_string())?;
        let mut errors = report["errors"].as_array().cloned().unwrap_or_default();
        let state = self.state.lock().map_err(|_| "validation lock poisoned")?;
        let fresh: Vec<_> = state
            .peers
            .iter()
            .filter(|(_, p)| {
                p.observation.revision == view.revision()
                    && p.at.elapsed() < Duration::from_secs(15)
            })
            .collect();
        let all_ids: Vec<_> = view
            .records()
            .filter(|r| r.kind == "node")
            .map(|r| &r.id)
            .collect();
        let mut complete = false;
        for ((participant, stream), seen) in &fresh {
            let observation = &seen.observation;
            let local = samepage_map::validation::report(view, &observation.bounds)
                .map_err(|e| e.to_string())?;
            for mut error in local["errors"].as_array().cloned().unwrap_or_default() {
                error["participant"] = json!(participant);
                error["stream"] = json!(stream);
                errors.push(error);
            }
            for error in &observation.errors {
                errors.push(
                    json!({"code":error.code,"message":error.message,"participant":participant,"stream":stream}),
                );
            }
            complete |= all_ids
                .iter()
                .all(|id| observation.bounds.contains_key(*id));
        }
        report["status"] = json!(if !errors.is_empty() {
            "failed"
        } else if complete {
            "passed"
        } else {
            "pending"
        });
        report["errors"] = json!(errors);
        report["revision"] = json!(view.revision());
        report["browser_peers"] = json!(fresh.len());
        report["generation"] = json!(state.generation);
        Ok(report)
    }
    pub async fn wait_since(&self, generation: u64, seconds: u64) {
        let notified = self.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.generation() != generation {
            return;
        }
        let _ = tokio::time::timeout(Duration::from_secs(seconds.min(60)), notified).await;
    }
}
