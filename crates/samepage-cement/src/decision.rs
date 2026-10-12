//! Reviewed intent and explicit structural checks. Execution belongs to the
//! repository's G8 adapter, outside the portable runtime kernel.
type AtlasError = String;
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
    /// When the host committed the draft, RFC 3339 UTC; empty for drafts
    /// recorded before the host stamped creation times.
    #[serde(default)]
    pub created_at: String,
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
