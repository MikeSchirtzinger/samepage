//! Ephemeral walkthrough attention stays in the native host. Cursor and
//! visual vocabulary are portable; browser animation never decides authority.
use ag_ui_surface::{Actor, AttentionMode, Caller, Participant, SemanticTargetRef, SemanticTargetService};
use samepage_capability::View;

pub fn attend(view: &View, flow_id: &str, actor: &Actor, service: Option<&SemanticTargetService>) -> Result<(), String> {
    if !matches!(actor.caller,Caller::Agent|Caller::Companion) { return Ok(()); }
    let Some(service) = service else { return Ok(()); };
    let flow = samepage_explain::flows(view).map_err(|e|e.to_string())?.into_iter().find(|f|f.definition.id==flow_id).ok_or("walkthrough attention flow is missing")?;
    let beat=flow.definition.beats.iter().find(|b|b.id==flow.state.current_beat).ok_or("walkthrough attention beat is missing")?;
    if let Some(action)=beat.actions.iter().find(|a|matches!(a.kind.as_str(),"point"|"reveal")) {
        let participant=Participant::agent(actor.participant_id.as_deref().unwrap_or("surface-agent"),actor.label.as_deref().unwrap_or("Agent"));
        service.attend(participant,if action.kind=="reveal"{AttentionMode::Reveal}else{AttentionMode::Pointer},SemanticTargetRef::new("map",&action.target_id),Some(beat.cue.chars().take(240).collect()))?;
    }
    Ok(())
}

/// An omitted optional owner cannot allow Map or Sketch to orphan its flows.
pub struct ExplainUnavailable;
impl samepage_doc::host::Verifier for ExplainUnavailable {
    fn verify(&self,c:&samepage_capability::Change<'_>)->Result<serde_json::Value,samepage_capability::CapError>{
        for r in c.before.records().filter(|r|r.kind==samepage_explain::KIND) {
            let definition: samepage_explain::ExplanationFlowDefinition=serde_json::from_str(samepage_map::prop_str(r,"definition").unwrap_or("")).map_err(|e|samepage_capability::CapError::new("needs_capability",format!("explain is unavailable; cannot inspect its preserved target references: {e}")))?;
            let referenced=definition.about.iter().chain(&definition.scene_ids).chain(definition.beats.iter().flat_map(|b|b.evidence.iter().map(|e|&e.target_id).chain(b.actions.iter().map(|a|&a.target_id)).chain(b.advance.transitions.iter().flat_map(|t|&t.target_ids))));
            if referenced.into_iter().any(|id|c.delta.deleted.contains(id)) {return Err(samepage_capability::CapError::new("needs_capability","explain capability is not loaded; removal would orphan its walkthrough targets"));}
        }
        Ok(serde_json::json!({}))
    }
}

/// Combine omission checks without replacing another lane's verifier.
pub struct MissingOwners { pub review: bool, pub sketch: bool, pub explain: bool }
impl samepage_doc::host::Verifier for MissingOwners {
    fn verify(&self,c:&samepage_capability::Change<'_>)->Result<serde_json::Value,samepage_capability::CapError> {
        super::sketch::MissingOwners {review:self.review,sketch:self.sketch}.verify(c)?;
        if !self.explain { ExplainUnavailable.verify(c)?; }
        Ok(serde_json::json!({}))
    }
}
