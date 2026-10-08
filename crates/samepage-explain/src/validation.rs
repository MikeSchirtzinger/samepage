use crate::model::*;
use crate::*;
type AtlasError = String;
fn text(v: &str, n: usize, f: &str) -> Result<String, String> {
    samepage_map::rules::text(v, n, f).map_err(|e| e.message)
}
fn one_of(v: &str, a: &[&str], f: &str) -> Result<(), String> {
    samepage_map::rules::one_of(v, a, f)
        .map(|_| ())
        .map_err(|e| e.message)
}
fn explanation_id(value: &str, field: &str) -> Result<String, AtlasError> {
    let value = text(value, 160, field)?;
    if value.is_empty()
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._:-".contains(character))
    {
        return Err(format!(
            "{field} must use 1 to 160 ASCII letters, numbers, dot, underscore, colon, or hyphen"
        ));
    }
    Ok(value)
}

pub fn validate_explanation_structure(
    definition: &ExplanationFlowDefinition,
) -> Result<(), AtlasError> {
    if definition.schema != EXPLANATION_FLOW_SCHEMA {
        return Err(format!(
            "explanation schema must be {EXPLANATION_FLOW_SCHEMA:?}"
        ));
    }
    explanation_id(&definition.id, "explanation id")?;
    let unique: std::collections::BTreeSet<_> = definition.scene_ids.iter().collect();
    if unique.len() != definition.scene_ids.len() {
        return Err("scene_ids must not contain duplicate objects".to_string());
    }
    if definition.about.len() > 64 || definition.scene_ids.len() > 512 {
        return Err("exploration scope exceeds 64 subjects or 512 visual objects".to_string());
    }
    if text(&definition.title, MAX_LABEL, "explanation title")?.is_empty() {
        return Err("explanation title must not be empty".to_string());
    }
    if text(&definition.goal, MAX_NOTE, "explanation goal")?.is_empty() {
        return Err("explanation goal must not be empty".to_string());
    }
    explanation_id(&definition.start, "explanation start")?;
    if definition.beats.is_empty() || definition.beats.len() > MAX_EXPLANATION_BEATS {
        return Err(format!(
            "an explanation needs 1 to {MAX_EXPLANATION_BEATS} beats"
        ));
    }

    let mut beat_ids = std::collections::BTreeSet::new();
    for beat in &definition.beats {
        let beat_id = explanation_id(&beat.id, "beat id")?;
        if !beat_ids.insert(beat_id.clone()) {
            return Err(format!("duplicate explanation beat {beat_id:?}"));
        }
        if text(&beat.title, MAX_LABEL, "beat title")?.is_empty() {
            return Err(format!("beat {beat_id:?} title must not be empty"));
        }
        if text(&beat.intent, MAX_TEXT, "beat intent")?.is_empty() {
            return Err(format!("beat {beat_id:?} intent must not be empty"));
        }
        if text(&beat.cue, MAX_TEXT, "beat cue")?.is_empty() {
            return Err(format!("beat {beat_id:?} cue must not be empty"));
        }
        if beat.evidence.is_empty() || beat.evidence.len() > MAX_EXPLANATION_EVIDENCE {
            return Err(format!(
                "beat {beat_id:?} needs 1 to {MAX_EXPLANATION_EVIDENCE} evidence bindings"
            ));
        }
        if beat.actions.is_empty() || beat.actions.len() > MAX_EXPLANATION_ACTIONS {
            return Err(format!(
                "beat {beat_id:?} needs 1 to {MAX_EXPLANATION_ACTIONS} visual actions"
            ));
        }
        for evidence in &beat.evidence {
            explanation_id(&evidence.target_id, "evidence target id")?;
            if text(&evidence.detail, MAX_TEXT, "evidence detail")?.is_empty() {
                return Err(format!("beat {beat_id:?} has an empty evidence detail"));
            }
        }
        let cue_chars = beat.cue.chars().count();
        let mut action_keys = std::collections::BTreeSet::new();
        for action in &beat.actions {
            one_of(
                &action.kind,
                EXPLANATION_ACTION_KINDS,
                "explanation action kind",
            )?;
            explanation_id(&action.target_id, "action target id")?;
            if !action_keys.insert((action.kind.clone(), action.target_id.clone())) {
                return Err(format!(
                    "beat {beat_id:?} repeats action {} on {}",
                    action.kind, action.target_id
                ));
            }
            if let Some([start, end]) = action.cue_span {
                let (start, end) = (start as usize, end as usize);
                if start >= end || end > cue_chars {
                    return Err(format!(
                        "beat {beat_id:?} action cue_span must be inside its {cue_chars}-character cue"
                    ));
                }
            }
        }

        one_of(
            &beat.advance.mode,
            EXPLANATION_ADVANCE_MODES,
            "explanation advance mode",
        )?;
        text(&beat.advance.prompt, MAX_TEXT, "advance prompt")?;
        if beat.advance.transitions.len() > MAX_EXPLANATION_TRANSITIONS {
            return Err(format!(
                "beat {beat_id:?} may have at most {MAX_EXPLANATION_TRANSITIONS} transitions"
            ));
        }
        match beat.advance.mode.as_str() {
            "terminal" if !beat.advance.transitions.is_empty() => {
                return Err(format!(
                    "terminal beat {beat_id:?} must not have transitions"
                ));
            }
            "continue" if beat.advance.transitions.len() != 1 => {
                return Err(format!(
                    "continue beat {beat_id:?} needs exactly one transition"
                ));
            }
            "choice" if beat.advance.transitions.len() < 2 => {
                return Err(format!(
                    "choice beat {beat_id:?} needs at least two transitions"
                ));
            }
            "agent" if beat.advance.transitions.is_empty() => {
                return Err(format!(
                    "agent beat {beat_id:?} needs at least one transition"
                ));
            }
            _ => {}
        }
        let mut transition_ids = std::collections::BTreeSet::new();
        for transition in &beat.advance.transitions {
            let transition_id = explanation_id(&transition.id, "transition id")?;
            if !transition_ids.insert(transition_id.clone()) {
                return Err(format!(
                    "beat {beat_id:?} repeats transition {transition_id:?}"
                ));
            }
            if text(&transition.label, MAX_LABEL, "transition label")?.is_empty() {
                return Err(format!(
                    "transition {transition_id:?} in beat {beat_id:?} needs a label"
                ));
            }
            if let Some(control) = transition.control.as_deref() {
                one_of(control, &["button"], "explanation transition control")?;
            }
            if let Some(next) = &transition.next {
                explanation_id(next, "transition next beat")?;
            }
            if transition.target_ids.len() > 32 || transition.phrases.len() > 32 {
                return Err(format!(
                    "transition {transition_id:?} may name at most 32 targets and 32 phrases"
                ));
            }
            let mut targets = std::collections::BTreeSet::new();
            for target_id in &transition.target_ids {
                let target_id = explanation_id(target_id, "transition target id")?;
                if !targets.insert(target_id.clone()) {
                    return Err(format!(
                        "transition {transition_id:?} repeats target {target_id:?}"
                    ));
                }
            }
            for phrase in &transition.phrases {
                if text(phrase, MAX_LABEL, "transition phrase")?.is_empty() {
                    return Err(format!(
                        "transition {transition_id:?} contains an empty phrase"
                    ));
                }
            }
        }
    }

    if !beat_ids.contains(&definition.start) {
        return Err(format!(
            "explanation start {:?} is not a beat",
            definition.start
        ));
    }
    for beat in &definition.beats {
        for transition in &beat.advance.transitions {
            if let Some(next) = &transition.next {
                if !beat_ids.contains(next) {
                    return Err(format!(
                        "transition {:?} from beat {:?} points to missing beat {next:?}",
                        transition.id, beat.id
                    ));
                }
            }
        }
    }

    fn visit(
        id: &str,
        beats: &std::collections::BTreeMap<&str, &ExplanationBeat>,
        visiting: &mut std::collections::BTreeSet<String>,
        visited: &mut std::collections::BTreeSet<String>,
    ) -> Result<(), AtlasError> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id.to_string()) {
            return Err(format!(
                "explanation flow contains a cycle through beat {id:?}; v1 flows must be finite"
            ));
        }
        let beat = beats
            .get(id)
            .ok_or_else(|| format!("missing explanation beat {id:?}"))?;
        for next in beat
            .advance
            .transitions
            .iter()
            .filter_map(|transition| transition.next.as_deref())
        {
            visit(next, beats, visiting, visited)?;
        }
        visiting.remove(id);
        visited.insert(id.to_string());
        Ok(())
    }

    let beats = definition
        .beats
        .iter()
        .map(|beat| (beat.id.as_str(), beat))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut visiting = std::collections::BTreeSet::new();
    let mut visited = std::collections::BTreeSet::new();
    visit(&definition.start, &beats, &mut visiting, &mut visited)?;
    if visited.len() != definition.beats.len() {
        let unreachable = definition
            .beats
            .iter()
            .filter(|beat| !visited.contains(&beat.id))
            .map(|beat| beat.id.clone())
            .collect::<Vec<_>>();
        return Err(format!(
            "explanation has unreachable beats: {}",
            unreachable.join(", ")
        ));
    }
    Ok(())
}

pub fn validate_explanation_state(
    definition: &ExplanationFlowDefinition,
    state: &ExplanationFlowState,
) -> Result<(), AtlasError> {
    if !definition
        .beats
        .iter()
        .any(|beat| beat.id == state.current_beat)
    {
        return Err(format!(
            "explanation state points to missing beat {:?}",
            state.current_beat
        ));
    }
    one_of(&state.status, EXPLANATION_STATUSES, "explanation status")?;
    if state.revision == 0 {
        return Err("explanation revision must be greater than zero".to_string());
    }
    if state.entered_revision == 0 || state.entered_revision > state.revision {
        return Err(
            "explanation entered_revision must be between one and its revision".to_string(),
        );
    }
    if state.last_selected_target_ids.len() > 32 {
        return Err("explanation state may record at most 32 selected targets".to_string());
    }
    text(&state.last_response, MAX_TEXT, "explanation last response")?;
    Ok(())
}
