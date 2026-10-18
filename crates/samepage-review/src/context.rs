//! The scope of a conversation, independent of the camera and containment.
//! Entries form a history DAG. Concurrent heads remain visible until a new
//! entry explicitly reconciles them. Both hosts use this projection.

use samepage_capability::{CapError, Op, Plan, View};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::{create, invalid, string};

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
    pub fn finalize(&mut self) -> Result<(), CapError> {
        self.entries.sort_by(|a, b| a.id.cmp(&b.id));
        let ids: BTreeSet<_> = self.entries.iter().map(|entry| &entry.id).collect();
        if ids.len() != self.entries.len() || self.entries.len() > 256 {
            return Err(invalid("invalid conversation context history"));
        }
        let mut replaced = BTreeSet::new();
        for entry in &self.entries {
            validate_text(&entry.question, &entry.assumptions)?;
            for parent in &entry.replaces {
                if parent == &entry.id || !ids.contains(parent) {
                    return Err(invalid(format!(
                        "context {:?} has an invalid predecessor",
                        entry.id
                    )));
                }
                replaced.insert(parent.clone());
            }
            // Follow predecessors to reject a cycle even when unrelated heads exist.
            let mut pending = entry.replaces.clone();
            let mut seen = BTreeSet::new();
            while let Some(parent) = pending.pop() {
                if parent == entry.id {
                    return Err(invalid("conversation context history contains a cycle"));
                }
                if seen.insert(parent.clone())
                    && let Some(previous) = self.entries.iter().find(|item| item.id == parent)
                {
                    pending.extend(previous.replaces.iter().cloned());
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

fn validate_text(question: &str, assumptions: &str) -> Result<(), CapError> {
    if question.trim().is_empty() || question.chars().count() > 2000 {
        return Err(invalid(
            "the shared question must contain 1 to 2000 characters",
        ));
    }
    if assumptions.chars().count() > 4000 {
        return Err(invalid("context assumptions exceed 4000 characters"));
    }
    Ok(())
}

pub fn project(view: &View) -> Result<ContextState, CapError> {
    let mut state = ContextState::default();
    for record in view.records().filter(|r| r.kind == KIND) {
        let input: ContextInput = serde_json::from_str(string(record, "definition"))
            .map_err(|e| invalid(format!("invalid conversation context: {e}")))?;
        state.entries.push(ContextEntry {
            id: record.id.clone(),
            question: input.question,
            altitude: input.altitude,
            subjects: input.subjects,
            assumptions: input.assumptions,
            replaces: input.replaces,
            created_by: string(record, "created_by").into(),
        });
    }
    state.finalize()?;
    Ok(state)
}

pub fn validate_input(input: &ContextInput, view: &View) -> Result<(), CapError> {
    validate_text(&input.question, &input.assumptions)?;
    if input.subjects.len() > 32
        || input.subjects.iter().collect::<BTreeSet<_>>().len() != input.subjects.len()
    {
        return Err(invalid("context needs at most 32 distinct subjects"));
    }
    for id in &input.subjects {
        if view
            .get(id)
            .is_none_or(|r| !matches!(r.kind.as_str(), "node" | "edge" | "shape"))
        {
            return Err(invalid(format!(
                "context subject {id:?} is not on this page"
            )));
        }
    }
    Ok(())
}

pub fn plan(input: &ContextInput, view: &View) -> Result<Plan, CapError> {
    validate_input(input, view)?;
    let state = project(view)?;
    let mut replaces = input.replaces.clone();
    replaces.sort();
    if replaces != state.heads {
        return Err(invalid(
            "conversation scope changed. Read the current context before replacing it",
        ));
    }
    if state.entries.len() >= 256 {
        return Err(invalid(
            "this page has reached its context history limit; start a new page",
        ));
    }
    let definition = serde_json::to_string(input).map_err(|e| invalid(e.to_string()))?;
    let op: Op = create(
        "context",
        KIND,
        [("definition".into(), definition.into())].into(),
    );
    Ok(Plan {
        ops: vec![op],
        readback: "Shared conversation context recorded; previous questions retained.".into(),
    })
}
