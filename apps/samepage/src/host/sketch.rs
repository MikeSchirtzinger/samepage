//! Native lift composition. Review stays optional: when marks need its owner,
//! absence is a named refusal before any mutation. Portable sketch never
//! impersonates a mark writer or depends on the review WASM.
use samepage_capability::{Batch, Call, CapError, Change, Target, View};
use samepage_doc::host::{BatchComposer, Verifier};
use samepage_map::prop_str;

pub struct SketchHost {
    pub review: bool,
}
impl BatchComposer for SketchHost {
    fn compose(&self, call: &Call, view: &View, mut batch: Batch) -> Result<Batch, CapError> {
        if call.action != "shape_lift" {
            return Ok(batch);
        }
        let id = call.args["id"]
            .as_str()
            .ok_or_else(|| CapError::new("invalid", "id required"))?;
        let (_, _, consumed) = samepage_sketch::lift_consumed(view, id)?;
        for from in consumed {
            let plan =
                samepage_review::retarget_marks(view, &from, Target::Ref("lifted-card".into()));
            if !plan.ops.is_empty() {
                if !self.review {
                    return Err(CapError::new(
                        "needs_capability",
                        "review capability is not loaded; lift cannot orphan its marks",
                    ));
                }
                batch.entries.push(samepage_sketch::entry("review", plan));
            }
        }
        Ok(batch)
    }
}
/// Bindings in preserved drawings still have an owner when sketch is omitted.
/// Refuse a removal that would orphan them, while unrelated Map edits work.
pub struct SketchUnavailable;
impl Verifier for SketchUnavailable {
    fn verify(&self, c: &Change<'_>) -> Result<serde_json::Value, CapError> {
        for r in c.before.records().filter(|r| r.kind == "shape") {
            if ["from", "to", "frame"]
                .iter()
                .any(|key| c.delta.deleted.contains(prop_str(r, key).unwrap_or("")))
            {
                return Err(CapError::new(
                    "needs_capability",
                    "sketch capability is not loaded; it must detach its bindings before removing their target",
                ));
            }
        }
        Ok(serde_json::json!({}))
    }
}
// Compose an omitted-owner check with the existing Review omission verifier,
// because each owner has one native verifier registration.
pub struct MissingOwners {
    pub review: bool,
    pub sketch: bool,
}
impl Verifier for MissingOwners {
    fn verify(&self, c: &Change<'_>) -> Result<serde_json::Value, CapError> {
        if !self.review {
            super::review::ReviewUnavailable.verify(c)?;
        }
        if !self.sketch {
            SketchUnavailable.verify(c)?;
        }
        Ok(serde_json::json!({}))
    }
}
