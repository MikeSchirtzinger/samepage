//! The scope of a conversation, independent of the camera and containment.
//! Entries form a history DAG. Concurrent heads remain visible until a new
//! entry explicitly reconciles them. Both hosts use this projection.

use ag_ui_canvas::scene::{Author, PropValue, Scene};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::{Atlas, AtlasError, read};

pub const KIND: &str = "understanding-context";

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Altitude {
    Purpose,
    #[default]
    System,
    Component,
    Implementation,
}

impl Altitude {
    pub fn label(self) -> &'static str {
        match self {
            Self::Purpose => "Purpose",
            Self::System => "System",
            Self::Component => "Component",
            Self::Implementation => "Implementation",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextInput {
    pub question: String,
    pub altitude: Altitude,
    #[serde(default)]
    pub subjects: Vec<String>,
    #[serde(default)]
    pub assumptions: String,
    /// Every currently observed head. This also makes reconciliation explicit.
    #[serde(default)]
    pub replaces: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextEntry {
    pub id: String,
    pub question: String,
    pub altitude: Altitude,
    pub subjects: Vec<String>,
    pub assumptions: String,
    pub replaces: Vec<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextState {
    pub entries: Vec<ContextEntry>,
    pub heads: Vec<String>,
    pub current: Option<ContextEntry>,
}

impl ContextState {
    pub fn finalize(&mut self) -> Result<(), AtlasError> {
        self.entries.sort_by(|a, b| a.id.cmp(&b.id));
        let ids: BTreeSet<_> = self.entries.iter().map(|entry| &entry.id).collect();
        if ids.len() != self.entries.len() || self.entries.len() > 256 {
            return Err("invalid conversation context history".into());
        }
        let mut replaced = BTreeSet::new();
        for entry in &self.entries {
            validate_text(&entry.question, &entry.assumptions)?;
            for parent in &entry.replaces {
                if parent == &entry.id || !ids.contains(parent) {
                    return Err(format!("context {:?} has an invalid predecessor", entry.id));
                }
                replaced.insert(parent.clone());
            }
            // Follow predecessors to reject a cycle even when unrelated heads exist.
            let mut pending = entry.replaces.clone();
            let mut seen = BTreeSet::new();
            while let Some(parent) = pending.pop() {
                if parent == entry.id {
                    return Err("conversation context history contains a cycle".into());
                }
                if seen.insert(parent.clone()) {
                    if let Some(previous) = self.entries.iter().find(|item| item.id == parent) {
                        pending.extend(previous.replaces.iter().cloned());
                    }
                }
            }
        }
        self.heads = self
            .entries
            .iter()
            .filter(|entry| !replaced.contains(&entry.id))
            .map(|entry| entry.id.clone())
            .collect();
        self.current = if self.heads.len() == 1 {
            self.entries
                .iter()
                .find(|entry| Some(&entry.id) == self.heads.first())
                .cloned()
        } else {
            None
        };
        Ok(())
    }

    pub fn describe(&self) -> String {
        if self.entries.is_empty() {
            return "\nCONVERSATION CONTEXT\nNo shared question yet. Set the question, altitude, and subjects before assuming what this page is about.\n".into();
        }
        if let Some(entry) = &self.current {
            format!(
                "\nCONVERSATION CONTEXT\n[{}] {}: {}\nSubjects: {}\nAssumptions: {}\nSet by: {}. Camera movement does not change this scope.\n",
                entry.id,
                entry.altitude.label(),
                entry.question,
                if entry.subjects.is_empty() {
                    "whole page".into()
                } else {
                    entry.subjects.join(", ")
                },
                if entry.assumptions.is_empty() {
                    "not specified"
                } else {
                    &entry.assumptions
                },
                entry.created_by
            )
        } else {
            let choices = self
                .entries
                .iter()
                .filter(|entry| self.heads.contains(&entry.id))
                .map(|entry| {
                    format!(
                        "- [{}] {}: {} (by {})",
                        entry.id,
                        entry.altitude.label(),
                        entry.question,
                        entry.created_by
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "\nCONVERSATION CONTEXT NEEDS RECONCILIATION\n{choices}\nChoose a shared scope explicitly, replacing every listed context.\n"
            )
        }
    }
}

fn validate_text(question: &str, assumptions: &str) -> Result<(), AtlasError> {
    if question.trim().is_empty() || question.chars().count() > 2000 {
        return Err("the shared question must contain 1 to 2000 characters".into());
    }
    if assumptions.chars().count() > 4000 {
        return Err("context assumptions exceed 4000 characters".into());
    }
    Ok(())
}

pub fn set(scene: &mut Scene, input: &ContextInput, author: &Author) -> Result<String, AtlasError> {
    validate_text(&input.question, &input.assumptions)?;
    let atlas = read(scene)?;
    let mut replaces = input.replaces.clone();
    replaces.sort();
    if replaces != atlas.context.heads {
        return Err(
            "conversation scope changed. Read the current context before replacing it".into(),
        );
    }
    if atlas.context.entries.len() >= 256 {
        return Err("this page has reached its context history limit; start a new page".into());
    }
    if input.subjects.len() > 32
        || input.subjects.iter().collect::<BTreeSet<_>>().len() != input.subjects.len()
    {
        return Err("context needs at most 32 distinct subjects".into());
    }
    for id in &input.subjects {
        if atlas.node(id).is_none()
            && atlas.shape(id).is_none()
            && !atlas.edges.iter().any(|edge| &edge.id == id)
        {
            return Err(format!("context subject {id:?} is not on this page"));
        }
    }
    let definition = serde_json::to_string(input).map_err(|error| error.to_string())?;
    scene
        .create_object_with_props(
            KIND,
            author.clone(),
            &[("definition", PropValue::Str(definition))],
        )
        .map(|id| id.into_string())
        .map_err(|error| format!("could not store conversation context: {error}"))
}

pub(crate) fn project(
    atlas: &mut Atlas,
    id: &str,
    definition: &str,
    created_by: String,
) -> Result<(), AtlasError> {
    let input: ContextInput = serde_json::from_str(definition)
        .map_err(|error| format!("invalid conversation context: {error}"))?;
    atlas.context.entries.push(ContextEntry {
        id: id.into(),
        question: input.question,
        altitude: input.altitude,
        subjects: input.subjects,
        assumptions: input.assumptions,
        replaces: input.replaces,
        created_by,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(question: &str, replaces: Vec<String>) -> ContextInput {
        ContextInput {
            question: question.into(),
            altitude: Altitude::System,
            subjects: vec![],
            assumptions: String::new(),
            replaces,
        }
    }

    #[test]
    fn a_context_change_retains_the_previous_question_and_refuses_a_stale_writer() {
        let mut scene = Scene::new();
        let first = set(
            &mut scene,
            &input("How do the peers share state?", vec![]),
            &Author::Human,
        )
        .unwrap();
        let mut detail = input(
            "What happens during a lost connection?",
            vec![first.clone()],
        );
        detail.altitude = Altitude::Implementation;
        let second = set(&mut scene, &detail, &Author::Human).unwrap();
        let before = scene.encode_full().unwrap();
        assert!(set(&mut scene, &detail, &Author::Human).is_err());
        assert_eq!(scene.encode_full().unwrap(), before);
        let state = read(&scene).unwrap().context;
        assert_eq!(state.heads, vec![second]);
        assert!(state.entries.iter().any(|entry| entry.id == first));
        assert_eq!(state.current.unwrap().altitude, Altitude::Implementation);
    }

    #[test]
    fn unrelated_context_heads_are_visible_until_explicitly_reconciled() {
        let mut state = ContextState {
            entries: vec![
                ContextEntry {
                    id: "a".into(),
                    question: "Why?".into(),
                    altitude: Altitude::Purpose,
                    subjects: vec![],
                    assumptions: String::new(),
                    replaces: vec![],
                    created_by: "human".into(),
                },
                ContextEntry {
                    id: "b".into(),
                    question: "How?".into(),
                    altitude: Altitude::Implementation,
                    subjects: vec![],
                    assumptions: String::new(),
                    replaces: vec![],
                    created_by: "agent".into(),
                },
            ],
            ..Default::default()
        };
        state.finalize().unwrap();
        assert!(state.current.is_none());
        assert!(state.describe().contains("RECONCILIATION"));
        let mut merged = state.entries.first().unwrap().clone();
        merged.id = "c".into();
        merged.replaces = vec!["a".into(), "b".into()];
        state.entries.push(merged);
        state.finalize().unwrap();
        assert_eq!(state.current.unwrap().id, "c");
    }

    #[test]
    fn an_unknown_scope_target_cannot_partially_change_the_page() {
        let mut scene = Scene::new();
        let before = scene.encode_full().unwrap();
        let mut request = input("Which component owns this?", vec![]);
        request.subjects.push("missing".into());
        assert!(set(&mut scene, &request, &Author::Human).is_err());
        assert_eq!(scene.encode_full().unwrap(), before);
    }

    #[test]
    fn scope_notifications_preserve_other_writers_without_repeating_my_own_edit() {
        let mut scene = Scene::new();
        let before = read(&scene).unwrap().digest();
        let first = set(&mut scene, &input("Which scope?", vec![]), &Author::Human).unwrap();
        let mine = read(&scene).unwrap().digest();
        assert!(crate::changes_for(&before, &mine, "human").is_empty());
        assert!(
            crate::changes_for(&before, &mine, "agent")
                .iter()
                .any(|line| line.contains("SHARED SCOPE CHANGED"))
        );
        set(
            &mut scene,
            &input("Which implementation?", vec![first]),
            &Author::Agent,
        )
        .unwrap();
        let after = read(&scene).unwrap().digest();
        assert!(
            crate::changes_for(&before, &after, "human")
                .iter()
                .any(|line| line.contains("SHARED SCOPE CHANGED"))
        );
    }

    #[test]
    fn real_crdt_peers_preserve_concurrent_scopes_and_question_history() {
        let mut left = Scene::new();
        let subject = crate::place_node(
            &mut left,
            &crate::NodePatch {
                label: Some("Peers".into()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .unwrap();
        let first = set(
            &mut left,
            &input("Why share understanding?", vec![]),
            &Author::Human,
        )
        .unwrap();
        let mut right = Scene::from_state(&left.encode_full().unwrap()).unwrap();
        let a = set(
            &mut left,
            &input("How do peers sync?", vec![first.clone()]),
            &Author::Human,
        )
        .unwrap();
        let mark = crate::mark(
            &mut left,
            &subject,
            "?",
            "Does offline editing fit here?",
            &Author::Human,
        )
        .unwrap();
        let b = set(
            &mut right,
            &input("Which protocol?", vec![first]),
            &Author::Agent,
        )
        .unwrap();
        let left_bytes = left.encode_full().unwrap();
        let right_bytes = right.encode_full().unwrap();
        left.apply_update(&right_bytes).unwrap();
        right.apply_update(&left_bytes).unwrap();
        assert_eq!(read(&left).unwrap().context, read(&right).unwrap().context);
        let conflict = read(&left).unwrap();
        assert!(conflict.context.current.is_none());
        assert_eq!(
            conflict
                .marks
                .iter()
                .find(|item| item.id == mark)
                .unwrap()
                .context_ids,
            vec![a.clone()]
        );
        let before = conflict.digest();
        set(
            &mut left,
            &input("What is the sync boundary?", vec![a, b]),
            &Author::Human,
        )
        .unwrap();
        let after = read(&left).unwrap();
        assert!(
            crate::changes(&before, &after.digest())
                .iter()
                .any(|line| line.contains("SHARED SCOPE CHANGED"))
        );
        assert!(
            after
                .describe()
                .contains("asked at System scope: How do peers sync?")
        );
    }
}
