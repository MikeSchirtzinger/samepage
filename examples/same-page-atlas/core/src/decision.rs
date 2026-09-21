//! Reviewed intent and explicit structural checks. Execution belongs to the
//! repository's G8 adapter, outside the portable runtime kernel.
use crate::{AtlasError, read};
use ag_ui_canvas::scene::{Author, PropValue, Scene};
use serde::{Deserialize, Serialize};

pub const KIND: &str = "decision-draft";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Alternative {
    pub option: String,
    pub reason_not_chosen: String,
}

/// A deliberately small, closed subset of G8's native structural backends.
/// These specifications are authored explicitly, never inferred from citations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "backend",
    content = "args",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum StructuralCheck {
    RustEnumShape(EnumShape),
    CargoMetadataNoDep(NoDependency),
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnumShape {
    pub file: String,
    pub enum_name: String,
    pub expected_variants: Vec<String>,
    pub expected_serde_rename_all: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NoDependency {
    pub denied: Vec<Dependency>,
    pub scope_crates: Vec<String>,
    pub build_config: BuildConfig,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Dependency {
    Exact(String),
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BuildConfig {
    Default,
    Features(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub id: String,
    pub requirement: String,
    pub check: StructuralCheck,
    /// Reviewed implementation files. G8 invalidates attestation if they change.
    pub evidence_files: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DraftInput {
    pub ids: Vec<String>,
    pub title: String,
    pub decision: String,
    pub rationale: String,
    pub alternatives: Vec<Alternative>,
    pub tradeoffs: String,
    pub consequences: String,
    pub checks: Vec<Check>,
    /// Requirements whose behavior still needs tests or human evaluation.
    #[serde(default)]
    pub not_enforced: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Draft {
    pub id: String,
    pub input: DraftInput,
    pub context_ids: Vec<String>,
    pub content: String,
    pub created_by: String,
}

fn relative_file(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}
impl DraftInput {
    pub fn validate(&self) -> Result<(), AtlasError> {
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > 65536
            || self.ids.is_empty()
            || self.ids.len() > 32
            || self.not_enforced.len() > 64
        {
            return Err("decision draft exceeds the bounded review size".into());
        }
        for (name, value) in [
            ("title", &self.title),
            ("decision", &self.decision),
            ("rationale", &self.rationale),
            ("tradeoffs", &self.tradeoffs),
            ("consequences", &self.consequences),
        ] {
            if value.trim().is_empty() || value.chars().count() > 8000 {
                return Err(format!("decision {name} must contain 1 to 8000 characters"));
            }
        }
        if self.alternatives.is_empty()
            || self.alternatives.len() > 20
            || self.alternatives.iter().any(|item| {
                item.option.trim().is_empty() || item.reason_not_chosen.trim().is_empty()
            })
        {
            return Err("record the alternatives and why each was not chosen".into());
        }
        if self.checks.is_empty() || self.checks.len() > 32 {
            return Err("cement needs 1 to 32 explicit G8 checks".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for check in &self.checks {
            if check.evidence_files.len() > 32
                || check
                    .evidence_files
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != check.evidence_files.len()
            {
                return Err("each check needs at most 32 distinct evidence files".into());
            }
            if check.id.is_empty()
                || !check
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
                || !ids.insert(&check.id)
            {
                return Err(
                    "G8 check ids must be distinct uppercase names with digits or hyphens".into(),
                );
            }
            if check.requirement.trim().is_empty()
                || check.evidence_files.is_empty()
                || !check.evidence_files.iter().all(|file| relative_file(file))
            {
                return Err(
                    "each check needs a requirement and repository-relative evidence files".into(),
                );
            }
            match &check.check {
                StructuralCheck::RustEnumShape(args) => {
                    if !relative_file(&args.file)
                        || args.enum_name.is_empty()
                        || args.expected_variants.is_empty()
                        || !check.evidence_files.contains(&args.file)
                    {
                        return Err("enum checks need a source file, enum name, variants, and that source among the evidence files".into());
                    }
                }
                StructuralCheck::CargoMetadataNoDep(args) => {
                    if args.denied.is_empty()
                        || args.scope_crates.is_empty()
                        || args
                            .denied
                            .iter()
                            .any(|Dependency::Exact(name)| name.is_empty())
                    {
                        return Err(
                            "dependency checks need explicit denied dependencies and scope crates"
                                .into(),
                        );
                    }
                }
            }
        }
        Ok(())
    }
}
pub fn save(scene: &mut Scene, input: &DraftInput, author: &Author) -> Result<String, AtlasError> {
    input.validate()?;
    let atlas = read(scene)?;
    if atlas.context.current.is_none() {
        return Err("record or reconcile the shared question before drafting a decision".into());
    }
    if atlas.decision_drafts.len() >= 128 {
        return Err("decision draft history is full".into());
    }
    let content = crate::claims::cement_content(&atlas, &input.ids)?;
    let draft = Draft {
        id: String::new(),
        input: input.clone(),
        context_ids: atlas.context.heads,
        content,
        created_by: author.as_str().into(),
    };
    let definition = serde_json::to_string(&draft).map_err(|error| error.to_string())?;
    scene
        .create_object_with_props(
            KIND,
            author.clone(),
            &[("definition", PropValue::Str(definition))],
        )
        .map(|id| id.into_string())
        .map_err(|error| error.to_string())
}
pub(crate) fn project(id: &str, definition: &str, created_by: String) -> Result<Draft, AtlasError> {
    let mut draft: Draft = serde_json::from_str(definition).map_err(|error| error.to_string())?;
    draft.input.validate()?;
    draft.id = id.into();
    draft.created_by = created_by;
    Ok(draft)
}
pub fn reviewed<'a>(
    atlas: &'a crate::Atlas,
    id: &str,
    selected: &[String],
) -> Result<&'a Draft, AtlasError> {
    let draft = atlas
        .decision_drafts
        .iter()
        .find(|draft| draft.id == id)
        .ok_or("decision draft not found")?;
    let mut actual = selected.to_vec();
    actual.sort();
    let mut expected = draft.input.ids.clone();
    expected.sort();
    if actual != expected
        || draft.context_ids != atlas.context.heads
        || draft.content != crate::claims::cement_content(atlas, selected)?
    {
        return Err("the page or conversational scope changed after this decision draft; review a new draft".into());
    }
    for mark in &atlas.marks {
        let relevant = selected.contains(&mark.target)
            || atlas
                .claims
                .iter()
                .any(|claim| claim.id == mark.target && selected.contains(&claim.about))
            || atlas.edges.iter().any(|edge| {
                edge.id == mark.target
                    && selected.contains(&edge.from)
                    && selected.contains(&edge.to)
            });
        if relevant && mark.answer.trim().is_empty() && matches!(mark.glyph.as_str(), "?" | "!") {
            return Err(format!(
                "answer the human's unresolved feedback {} before cementing: {}",
                mark.id, mark.text
            ));
        }
    }
    Ok(draft)
}
