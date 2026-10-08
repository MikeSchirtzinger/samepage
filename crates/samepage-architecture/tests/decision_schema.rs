// Retained decision schema used only by the restored-peer ordering test. Cement owns it.
#![allow(dead_code)]
use serde::{Deserialize, Serialize};
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
