//! Startup-compiled JSON Schema enforcement for [`ActionDef`](crate::ActionDef).
//!
//! Discovery and execution deliberately share the exact same schema value:
//! applications declare it once on `ActionDef`, startup validates and compiles
//! every effective action, and the shared dispatcher reuses the compiled
//! validator before calling `ActionDef::apply`.

use std::collections::HashMap;

use jsonschema::Validator;
use serde_json::Value;

use crate::ActionDef;

/// The compiled argument contract for one effective [`Surface`](crate::Surface).
///
/// This is separate from the MCP catalog: MCP is only one discovery projection,
/// while these validators cover every action audience and every caller.
#[derive(Debug)]
pub(crate) struct ActionSchemas {
    validators: HashMap<String, Validator>,
}

impl ActionSchemas {
    /// Validate and compile every effective action schema before the server
    /// binds or starts a provider.
    pub(crate) fn compile(actions: &[ActionDef]) -> Result<Self, String> {
        let mut validators = HashMap::with_capacity(actions.len());

        for action in actions {
            if !valid_action_name(&action.name) {
                return Err(format!(
                    "action name {:?} must be 1-64 ASCII letters, digits, underscores, or hyphens",
                    action.name
                ));
            }
            if validators.contains_key(&action.name) {
                return Err(format!(
                    "action {:?} is declared more than once",
                    action.name
                ));
            }

            let schema = &action.parameters;
            if !schema.is_object() || schema.get("type").and_then(Value::as_str) != Some("object") {
                return Err(format!(
                    "action {:?} input schema must be an object schema with type=object",
                    action.name
                ));
            }

            validate_schema(schema).map_err(|error| {
                format!(
                    "action {:?} has an invalid input schema: {error}",
                    action.name
                )
            })?;
            let validator = compile_schema(schema).map_err(|error| {
                format!(
                    "action {:?} input schema could not be compiled: {error}",
                    action.name
                )
            })?;
            validators.insert(action.name.clone(), validator);
        }

        Ok(Self { validators })
    }

    pub(crate) fn validate(&self, name: &str, arguments: &Value) -> Result<(), String> {
        let validator = self
            .validators
            .get(name)
            .ok_or_else(|| format!("action {name:?} has no compiled input schema"))?;
        let errors = validator
            .iter_errors(arguments)
            .take(4)
            .map(|error| {
                let path = error.instance_path().to_string();
                if path.is_empty() {
                    error.to_string()
                } else {
                    format!("{path}: {error}")
                }
            })
            .collect::<Vec<_>>();

        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "invalid arguments for action {name:?}: {}",
                errors.join("; ")
            ))
        }
    }
}

fn valid_action_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// MCP 2025-06-18 did not name an embedded-schema dialect. Use JSON Schema
/// 2020-12 when `$schema` is absent, while honoring any explicit dialect the
/// validator supports. Keeping the two branches explicit prevents dependency
/// default drift from silently changing ActionDef semantics later.
fn validate_schema(schema: &Value) -> Result<(), String> {
    let result = if schema.get("$schema").is_some() {
        jsonschema::meta::validate(schema)
    } else {
        jsonschema::draft202012::meta::validate(schema)
    };
    result.map_err(|error| format_schema_error(&error))
}

fn compile_schema(schema: &Value) -> Result<Validator, String> {
    let result = if schema.get("$schema").is_some() {
        jsonschema::validator_for(schema)
    } else {
        jsonschema::draft202012::options().build(schema)
    };
    result.map_err(|error| format_schema_error(&error))
}

fn format_schema_error(error: &jsonschema::ValidationError<'_>) -> String {
    let path = error.instance_path().to_string();
    if path.is_empty() {
        error.to_string()
    } else {
        format!("{path}: {error}")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{ActionAudience, Effect, ToolDef};

    use super::*;

    fn action(name: &str, schema: Value) -> ToolDef {
        ToolDef::new(name, "schema test", schema, |_args| {
            Effect::Query(Box::new(|_state| Ok("ok".to_string())))
        })
    }

    #[test]
    fn compiles_every_audience_and_enforces_nested_arguments() {
        let schemas = ActionSchemas::compile(&[
            action(
                "human",
                json!({
                    "type": "object",
                    "properties": {
                        "config": {
                            "type": "object",
                            "properties": { "count": { "type": "integer", "minimum": 1 } },
                            "required": ["count"],
                            "additionalProperties": false
                        }
                    },
                    "required": ["config"],
                    "additionalProperties": false
                }),
            )
            .audience(ActionAudience::Human),
            action(
                "agent",
                json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            )
            .audience(ActionAudience::Agent),
        ])
        .unwrap();

        assert_eq!(
            schemas.validate("human", &json!({ "config": { "count": 2 } })),
            Ok(())
        );
        let error = schemas
            .validate("human", &json!({ "config": { "count": 0, "extra": true } }))
            .unwrap_err();
        assert!(error.contains("/config/count"), "{error}");
        assert!(error.contains("minimum"), "{error}");
        assert!(
            error.to_lowercase().contains("additional properties"),
            "{error}"
        );
    }

    #[test]
    fn rejects_invalid_human_only_schema_during_startup_compilation() {
        let invalid = action(
            "human_invalid",
            json!({ "type": "object", "required": "not-an-array" }),
        )
        .human_only();
        let error = ActionSchemas::compile(&[invalid]).unwrap_err();
        assert!(error.contains("human_invalid"), "{error}");
        assert!(error.contains("invalid input schema"), "{error}");
    }

    #[test]
    fn rejects_non_object_duplicate_unsupported_and_unresolved_schemas() {
        let non_object =
            ActionSchemas::compile(&[action("bad", json!({ "type": "string" }))]).unwrap_err();
        assert!(non_object.contains("type=object"), "{non_object}");

        let duplicate = ActionSchemas::compile(&[
            action("same", json!({ "type": "object" })),
            action("same", json!({ "type": "object" })),
        ])
        .unwrap_err();
        assert!(duplicate.contains("more than once"), "{duplicate}");

        let unsupported = ActionSchemas::compile(&[action(
            "unsupported",
            json!({ "$schema": "https://example.invalid/schema", "type": "object" }),
        )])
        .unwrap_err();
        assert!(
            unsupported.contains("invalid input schema"),
            "{unsupported}"
        );
        assert!(
            unsupported.contains("example.invalid/schema"),
            "{unsupported}"
        );

        let unresolved = ActionSchemas::compile(&[action(
            "unresolved",
            json!({
                "type": "object",
                "properties": { "value": { "$ref": "https://example.invalid/value" } }
            }),
        )])
        .unwrap_err();
        assert!(unresolved.contains("could not be compiled"), "{unresolved}");
    }

    #[test]
    fn rejects_action_names_that_provider_protocols_cannot_advertise() {
        for name in ["", "bad name", "bad.name", "é", &"x".repeat(65)] {
            let error =
                ActionSchemas::compile(&[action(name, json!({ "type": "object" }))]).unwrap_err();
            assert!(error.contains("1-64 ASCII"), "{error}");
        }
        assert!(
            ActionSchemas::compile(&[action("safe_name-64", json!({ "type": "object" }))]).is_ok()
        );
    }

    #[test]
    fn allows_extra_properties_unless_the_schema_forbids_them() {
        let open = ActionSchemas::compile(&[action(
            "open",
            json!({ "type": "object", "properties": {} }),
        )])
        .unwrap();
        assert_eq!(open.validate("open", &json!({ "extra": true })), Ok(()));
    }

    #[test]
    fn honors_a_supported_explicit_dialect_and_internal_references() {
        let schemas = ActionSchemas::compile(&[action(
            "draft7",
            json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "definitions": {
                    "nonempty": { "type": "string", "minLength": 1 }
                },
                "properties": {
                    "name": { "$ref": "#/definitions/nonempty" }
                },
                "required": ["name"],
                "additionalProperties": false
            }),
        )])
        .unwrap();

        assert_eq!(
            schemas.validate("draft7", &json!({ "name": "Ada" })),
            Ok(())
        );
        let error = schemas
            .validate("draft7", &json!({ "name": "" }))
            .unwrap_err();
        assert!(error.contains("/name"), "{error}");
        assert!(error.contains("shorter"), "{error}");
    }
}
