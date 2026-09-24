//! Versioned, host-pinned browser artifact declarations.
//! The browser verifies bytes and the artifact's exported identity before use.

use serde::Serialize;
use serde_json::Value;

use crate::{ActionAudience, ActionDef};

pub const CAPABILITY_SCHEMA_VERSION: u32 = 1;
const CAPABILITY_ABI: &str = "samepage.capability.v2";

/// The one browser document, independent of the C1 capability ABI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentMount {
    pub pkg_name: String,
    pub js: String,
    pub js_sha256: String,
    pub js_bytes: u64,
    pub wasm: String,
    pub sha256: String,
    pub bytes: u64,
    pub abi: String,
    pub layout_version: u32,
}

impl DocumentMount {
    pub fn validate(&self) -> Result<(), String> {
        if self.pkg_name.trim().is_empty()
            || self.abi != "samepage.document.v1"
            || self.layout_version != 1
        {
            return Err("document mount has unsupported package, ABI or layout version".into());
        }
        ArtifactFile {
            url: self.js.clone(),
            sha256: self.js_sha256.clone(),
            bytes: self.js_bytes,
        }
        .validate()?;
        ArtifactFile {
            url: self.wasm.clone(),
            sha256: self.sha256.clone(),
            bytes: self.bytes,
        }
        .validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactFile {
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
}

impl ArtifactFile {
    fn validate(&self) -> Result<(), String> {
        if !self.url.starts_with('/')
            || self.url.starts_with("//")
            || self.url.contains('%')
            || self.url.contains("..")
            || self.url.chars().any(|ch| matches!(ch, '?' | '#' | '\\'))
            || self
                .url
                .split('/')
                .any(|segment| segment == ".." || segment == ".")
        {
            return Err(format!(
                "artifact URL {:?} must be a local absolute path",
                self.url
            ));
        }
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("artifact {:?} has invalid SHA-256", self.url));
        }
        if self.bytes == 0 {
            return Err(format!("artifact {:?} has zero bytes", self.url));
        }
        Ok(())
    }
}

/// A pure capability package. The document has its own top-level mount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityArtifact {
    pub kind: String,
    pub abi: String,
    pub entry: ArtifactFile,
    pub glue: ArtifactFile,
    pub wasm: ArtifactFile,
    /// Host-compiled descriptor, compared with the artifact export in Chrome.
    pub descriptor: Value,
}

impl CapabilityArtifact {
    pub fn validate(&self, id: &str, version: &str, module: Option<&str>) -> Result<(), String> {
        if self.kind != "capability" {
            return Err(format!("{id}: unsupported artifact kind {:?}", self.kind));
        }
        if self.abi != CAPABILITY_ABI {
            return Err(format!("{id}: unsupported artifact ABI {:?}", self.abi));
        }
        for file in [&self.entry, &self.glue, &self.wasm] {
            file.validate()?;
        }
        if module != Some(self.entry.url.as_str()) {
            return Err(format!(
                "{id}: entry URL disagrees with ClientModule.module"
            ));
        }
        if self.descriptor.get("id").and_then(Value::as_str) != Some(id)
            || self.descriptor.get("version").and_then(Value::as_str) != Some(version)
            || self.descriptor.get("abi").and_then(Value::as_str) != Some(self.abi.as_str())
        {
            return Err(format!(
                "{id}: artifact descriptor identity disagrees with host"
            ));
        }
        validate_capability_descriptor(&self.descriptor, id)?;
        Ok(())
    }
}

fn validate_capability_descriptor(descriptor: &Value, id: &str) -> Result<(), String> {
    if descriptor
        .get("artifact")
        .and_then(|v| v.get("type"))
        .and_then(Value::as_str)
        != Some("wasm")
        || descriptor
            .get("schema")
            .and_then(Value::as_u64)
            .is_none_or(|n| n == 0)
    {
        return Err(format!(
            "{id}: capability must declare a wasm artifact and positive schema"
        ));
    }
    for field in [
        "kinds",
        "props",
        "reads",
        "depends_on",
        "actions",
        "visual_actions",
        "triggers",
    ] {
        if !descriptor.get(field).is_some_and(Value::is_array) {
            return Err(format!("{id}: descriptor missing {field} array"));
        }
    }
    for field in ["kinds", "props"] {
        let Some(claims) = descriptor[field].as_array() else {
            return Err(format!("{id}: descriptor missing {field} array"));
        };
        for claim in claims {
            if claim
                .get("kind")
                .and_then(Value::as_str)
                .is_none_or(|kind| kind.trim().is_empty())
                || claim
                    .get("keys")
                    .and_then(Value::as_array)
                    .is_none_or(|keys| {
                        keys.iter()
                            .any(|key| key.as_str().is_none_or(|key| key.trim().is_empty()))
                    })
            {
                return Err(format!(
                    "{id}: descriptor has invalid {field} claim {claim}"
                ));
            }
        }
    }
    if descriptor["reads"].as_array().is_none_or(|reads| {
        reads
            .iter()
            .any(|kind| kind.as_str().is_none_or(|kind| kind.trim().is_empty()))
    }) {
        return Err(format!("{id}: descriptor has invalid reads declaration"));
    }
    let Some(triggers) = descriptor["triggers"].as_array() else {
        return Err(format!("{id}: triggers missing"));
    };
    if triggers.is_empty() {
        return Err(format!("{id}: descriptor has no activation trigger"));
    }
    for trigger in triggers {
        let Some(fields) = trigger.as_object() else {
            return Err(format!("{id}: invalid activation trigger {trigger}"));
        };
        let on = trigger.get("on").and_then(Value::as_str);
        let valid = match on {
            Some("always") => fields.len() == 1,
            Some("kind" | "tool" | "route") => {
                fields.len() == 2
                    && trigger
                        .get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|name| !name.trim().is_empty())
            }
            Some("property") => {
                fields.len() == 3
                    && trigger
                        .get("kind")
                        .and_then(Value::as_str)
                        .is_some_and(|kind| !kind.trim().is_empty())
                    && trigger
                        .get("key")
                        .and_then(Value::as_str)
                        .is_some_and(|key| !key.trim().is_empty())
            }
            _ => false,
        };
        if !valid {
            return Err(format!("{id}: invalid activation trigger {trigger}"));
        }
        if on == Some("kind") {
            let kind = trigger["name"].as_str().unwrap_or_default();
            let involved = ["kinds", "props"].iter().any(|field| {
                descriptor[*field]
                    .as_array()
                    .is_some_and(|claims| claims.iter().any(|claim| claim["kind"] == kind))
            }) || descriptor["reads"]
                .as_array()
                .is_some_and(|reads| reads.iter().any(|read| read == kind));
            if !involved {
                return Err(format!(
                    "{id}: kind trigger {kind} is neither owned, extended nor read"
                ));
            }
        }
        if on == Some("property") {
            let kind = trigger["kind"].as_str().unwrap_or_default();
            let key = trigger["key"].as_str().unwrap_or_default();
            let owns = ["kinds", "props"].iter().any(|field| {
                descriptor[*field].as_array().is_some_and(|claims| {
                    claims.iter().any(|claim| {
                        claim["kind"] == kind
                            && claim["keys"]
                                .as_array()
                                .is_some_and(|keys| keys.iter().any(|claimed| claimed == key))
                    })
                })
            });
            if !owns {
                return Err(format!(
                    "{id}: property trigger {kind}.{key} is neither owned nor extended"
                ));
            }
        }
    }
    Ok(())
}

fn validate_native_descriptor(descriptor: &Value, id: &str, version: &str) -> Result<(), String> {
    if descriptor.get("id").and_then(Value::as_str) != Some(id)
        || descriptor.get("version").and_then(Value::as_str) != Some(version)
        || descriptor.get("abi").and_then(Value::as_str) != Some(CAPABILITY_ABI)
        || descriptor
            .get("artifact")
            .and_then(|v| v.get("type"))
            .and_then(Value::as_str)
            != Some("native_only")
        || descriptor
            .get("artifact")
            .and_then(|v| v.get("reason"))
            .and_then(Value::as_str)
            .is_none_or(|s| s.trim().is_empty())
        || !descriptor
            .get("kinds")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        || !descriptor
            .get("props")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        || !descriptor
            .get("reads")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        || !descriptor
            .get("actions")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        || !descriptor
            .get("visual_actions")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        || !descriptor.get("depends_on").is_some_and(Value::is_array)
        || !descriptor
            .get("triggers")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    {
        return Err(format!("{id}: invalid NativeOnly host adapter descriptor"));
    }
    Ok(())
}

pub(crate) fn validate_artifact_set<'a>(
    modules: impl IntoIterator<
        Item = (
            &'a str,
            &'a str,
            Option<&'a CapabilityArtifact>,
            Option<&'a Value>,
        ),
    >,
    actions: &[ActionDef],
) -> Result<(), String> {
    let modules: Vec<_> = modules.into_iter().collect();
    for (position, (id, version, artifact, native)) in modules.iter().enumerate() {
        let descriptor = match (artifact, native) {
            (Some(artifact), None) => {
                artifact.validate(id, version, Some(artifact.entry.url.as_str()))?;
                &artifact.descriptor
            }
            (None, Some(native)) => {
                validate_native_descriptor(native, id, version)?;
                native
            }
            (Some(_), Some(_)) => return Err(format!("{id}: both wasm and NativeOnly declared")),
            (None, None) => continue,
        };
        let Some(dependencies) = descriptor["depends_on"].as_array() else {
            return Err(format!("{id}: dependencies missing"));
        };
        for dep in dependencies {
            let dep_id = dep
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{id}: invalid dependency id"))?;
            let dep_version = dep
                .get("version")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{id}: invalid dependency version"))?;
            let resolved = modules[..position]
                .iter()
                .find(|(other_id, other_version, _, _)| {
                    *other_id == dep_id && *other_version == dep_version
                });
            if resolved
                .is_none_or(|(_, _, artifact, native)| artifact.is_none() && native.is_none())
            {
                return Err(format!(
                    "{id}: dependency {dep_id}@{dep_version} has no verified artifact or explicit NativeOnly host adapter"
                ));
            }
        }
        let Some(declarations) = descriptor["actions"].as_array() else {
            return Err(format!("{id}: actions missing"));
        };
        for declaration in declarations {
            let name = declaration
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{id}: action missing name"))?;
            let action = actions
                .iter()
                .find(|action| action.name == name)
                .ok_or_else(|| format!("{id}: descriptor action {name} has no host ActionDef"))?;
            if declaration.get("input_schema") != Some(&action.parameters) {
                return Err(format!(
                    "{id}: action {name} input_schema differs from host"
                ));
            }
            let audience = match action.audience {
                ActionAudience::Human => "human",
                ActionAudience::Agent => "agent",
                ActionAudience::Both => "both",
            };
            if declaration.get("audience").and_then(Value::as_str) != Some(audience) {
                return Err(format!("{id}: action {name} audience differs from host"));
            }
            let aliases = declaration
                .get("aliases")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("{id}: action {name} aliases missing"))?;
            if aliases
                != &action
                    .aliases
                    .iter()
                    .map(|alias| Value::String(alias.clone()))
                    .collect::<Vec<_>>()
            {
                return Err(format!("{id}: action {name} aliases differ from host"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn file(url: &str) -> ArtifactFile {
        ArtifactFile {
            url: url.into(),
            sha256: "a".repeat(64),
            bytes: 4,
        }
    }

    fn artifact(id: &str) -> CapabilityArtifact {
        CapabilityArtifact {
            kind: "capability".into(),
            abi: CAPABILITY_ABI.into(),
            entry: file(&format!("/capabilities/{id}/index.js")),
            glue: file(&format!("/capabilities/{id}/{id}.js")),
            wasm: file(&format!("/capabilities/{id}/{id}_bg.wasm")),
            descriptor: json!({
                "abi":CAPABILITY_ABI, "id":id, "version":"1", "schema":1,
                "artifact":{"type":"wasm"}, "kinds":[], "props":[], "reads":[],
                "depends_on":[], "actions":[], "visual_actions":[],
                "triggers":[{"on":"always"}]
            }),
        }
    }

    #[test]
    fn descriptor_identity_trigger_and_dependency_are_checked_at_registration() {
        let valid = artifact("map");
        assert_eq!(
            validate_artifact_set([("map", "1", Some(&valid), None)], &[]),
            Ok(())
        );
        let mut wrong = valid.clone();
        wrong.descriptor["id"] = json!("another");
        assert!(validate_artifact_set([("map", "1", Some(&wrong), None)], &[]).is_err());
        wrong = valid.clone();
        wrong.descriptor["triggers"] = json!([{"on":"kind", "name":"unknown"}]);
        assert!(validate_artifact_set([("map", "1", Some(&wrong), None)], &[]).is_err());
        wrong = valid.clone();
        wrong.descriptor["depends_on"] = json!([{"id":"doc", "version":"1", "reason":"x"}]);
        assert!(validate_artifact_set([("map", "1", Some(&wrong), None)], &[]).is_err());
        wrong = valid.clone();
        wrong.wasm.sha256 = "flipped".into();
        assert!(validate_artifact_set([("map", "1", Some(&wrong), None)], &[]).is_err());
    }

    #[test]
    fn property_tool_and_root_route_triggers_follow_v2_contract() {
        let mut item = artifact("diagram");
        item.descriptor["kinds"] = json!([{
            "kind":"edge", "keys":["event"], "reason":"typed transitions"
        }]);
        item.descriptor["actions"] = json!([{
            "name":"diagram_draw", "aliases":[], "audience":"both",
            "effect":"mutate", "description":"draw",
            "input_schema":{"type":"object"}
        }]);
        item.descriptor["triggers"] = json!([
            {"on":"tool", "name":"diagram"},
            {"on":"route", "name":"/"},
            {"on":"property", "kind":"edge", "key":"event"}
        ]);
        let host = ActionDef::new("diagram_draw", "draw", json!({"type":"object"}), |_| {
            crate::Effect::Reject("unused".into())
        });
        assert_eq!(
            validate_artifact_set(
                [("diagram", "1", Some(&item), None)],
                std::slice::from_ref(&host)
            ),
            Ok(())
        );

        let mut wrong = item.clone();
        wrong.descriptor["triggers"][2]["key"] = json!("guard");
        assert!(validate_artifact_set(
            [("diagram", "1", Some(&wrong), None)],
            std::slice::from_ref(&host)
        )
        .is_err());

        wrong = item.clone();
        wrong.descriptor["triggers"][2]["name"] = json!("event");
        assert!(validate_artifact_set(
            [("diagram", "1", Some(&wrong), None)],
            std::slice::from_ref(&host)
        )
        .is_err());
    }

    #[test]
    fn v1_capability_artifact_is_refused() {
        let mut item = artifact("map");
        item.abi = "samepage.capability.v1".into();
        item.descriptor["abi"] = json!("samepage.capability.v1");
        assert!(validate_artifact_set([("map", "1", Some(&item), None)], &[]).is_err());
    }

    #[test]
    fn input_schema_and_aliases_must_match_host_action() {
        let host = ActionDef::new(
            "card_place",
            "place",
            json!({"type":"object","required":["title"]}),
            |_| crate::Effect::Reject("unused".into()),
        )
        .alias("atlas_place");
        let mut item = artifact("map");
        item.descriptor["actions"] = json!([{
            "name":"card_place", "aliases":["atlas_place"], "audience":"both",
            "effect":"mutate", "description":"place",
            "input_schema":{"type":"object","required":["title"]}
        }]);
        assert_eq!(
            validate_artifact_set(
                [("map", "1", Some(&item), None)],
                std::slice::from_ref(&host)
            ),
            Ok(())
        );
        item.descriptor["actions"][0]["input_schema"] = json!({"type":"object"});
        assert!(validate_artifact_set(
            [("map", "1", Some(&item), None)],
            std::slice::from_ref(&host)
        )
        .is_err());
        item.descriptor["actions"][0]["input_schema"] = host.parameters.clone();
        item.descriptor["actions"][0]["aliases"] = json!([]);
        assert!(validate_artifact_set([("map", "1", Some(&item), None)], &[host]).is_err());
    }

    #[test]
    fn explicit_native_only_dependency_resolves_without_a_wasm_package() {
        let native = json!({
            "abi":CAPABILITY_ABI, "id":"adapter", "version":"1", "schema":1,
            "artifact":{"type":"native_only", "reason":"host provides the adapter"},
            "kinds":[], "props":[], "reads":[], "depends_on":[], "actions":[],
            "visual_actions":[], "triggers":[]
        });
        let mut map = artifact("map");
        map.descriptor["depends_on"] = json!([{
            "id":"adapter", "version":"1", "reason":"host adapter"
        }]);
        assert_eq!(
            validate_artifact_set(
                [
                    ("adapter", "1", None, Some(&native)),
                    ("map", "1", Some(&map), None),
                ],
                &[]
            ),
            Ok(())
        );
        assert!(validate_artifact_set(
            [("adapter", "1", None, None), ("map", "1", Some(&map), None),],
            &[]
        )
        .is_err());
        assert!(validate_artifact_set(
            [
                ("adapter", "2", None, Some(&native)),
                ("map", "1", Some(&map), None),
            ],
            &[]
        )
        .is_err());
        let mut invalid = native.clone();
        invalid["kinds"] = json!(["node"]);
        assert!(validate_artifact_set(
            [
                ("adapter", "1", None, Some(&invalid)),
                ("map", "1", Some(&map), None),
            ],
            &[]
        )
        .is_err());
    }

    #[test]
    fn native_adapter_may_depend_on_an_earlier_verified_wasm_package() {
        let tags = artifact("tags");
        let native = json!({
            "abi":CAPABILITY_ABI, "id":"adapter", "version":"1", "schema":1,
            "artifact":{"type":"native_only", "reason":"host adapter"},
            "kinds":[], "props":[], "reads":[],
            "depends_on":[{"id":"tags", "version":"1", "reason":"tag index"}],
            "actions":[], "visual_actions":[], "triggers":[]
        });
        let mut spare = artifact("spare");
        spare.descriptor["depends_on"] = json!([{
            "id":"adapter", "version":"1", "reason":"host adapter"
        }]);
        assert_eq!(
            validate_artifact_set(
                [
                    ("tags", "1", Some(&tags), None),
                    ("adapter", "1", None, Some(&native)),
                    ("spare", "1", Some(&spare), None),
                ],
                &[]
            ),
            Ok(())
        );
        let mut wrong = native.clone();
        wrong["depends_on"][0]["version"] = json!("2");
        assert!(validate_artifact_set(
            [
                ("tags", "1", Some(&tags), None),
                ("adapter", "1", None, Some(&wrong)),
                ("spare", "1", Some(&spare), None),
            ],
            &[]
        )
        .is_err());
    }
}
