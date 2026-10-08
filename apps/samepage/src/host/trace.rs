//! Native optional activity adapter. Identity and durable operational events
//! remain stamped by ag-ui-surface; this ring contains diagnostic spans only.
use samepage_capability::activity::{ActivityRecord, ActivitySink};
use samepage_capability::{Call, Capability, View};
use samepage_trace::{Ring, Trace};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
pub struct TraceHost {
    ring: Mutex<Ring>,
}
impl TraceHost {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            ring: Mutex::new(Ring::default()),
        })
    }
    pub fn invoke(&self, action: &str, args: Value) -> Result<String, String> {
        Trace
            .plan(
                &Call {
                    action: action.into(),
                    args: args.clone(),
                },
                &View::new("trace", []).unwrap(),
            )
            .map_err(|e| e.to_string())?;
        let operation = match action {
            "trace_read" => "snapshot",
            "trace_clear" => "clear",
            "trace_benchmark" => "benchmark",
            _ => return Err("unknown trace action".into()),
        };
        let mut input = args;
        input["operation"] = json!(operation);
        let response = samepage_trace::operate(
            &mut *self.ring.lock().map_err(|e| e.to_string())?,
            &input.to_string(),
        );
        let value: Value = serde_json::from_str(&response).map_err(|e| e.to_string())?;
        if !value["error"].is_null() {
            return Err(value["error"].to_string());
        }
        Ok(response)
    }
}
impl ActivitySink for TraceHost {
    fn emit(&self, record: ActivityRecord) {
        if let Ok(mut ring) = self.ring.lock() {
            let _ = ring.record(record);
        }
    }
}
