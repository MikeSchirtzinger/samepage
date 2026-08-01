//! Fail-closed environment configuration helpers for AG-UI applications.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum EnvConfigError {
    #[error("{name} is not valid Unicode")]
    NotUnicode { name: &'static str },
    #[error("{name} must not be empty")]
    Empty { name: &'static str },
    #[error("{name} must be a valid port, got {value:?}: {message}")]
    InvalidPort {
        name: &'static str,
        value: String,
        message: String,
    },
    #[error("{name} must be a boolean (true/false, on/off, yes/no, or 1/0), got {value:?}")]
    InvalidBoolean { name: &'static str, value: String },
    #[error("{name} enables unavailable command {command:?}")]
    UnavailableCommand { name: &'static str, command: String },
}

/// Read a present environment value, rejecting non-Unicode and blank input.
pub fn optional_nonempty(name: &'static str) -> Result<Option<String>, EnvConfigError> {
    let value = std::env::var_os(name);
    normalize_optional_os(name, value.as_deref())
}

pub(crate) fn normalize_optional_os(
    name: &'static str,
    value: Option<&OsStr>,
) -> Result<Option<String>, EnvConfigError> {
    let value = value
        .map(|raw| raw.to_str().ok_or(EnvConfigError::NotUnicode { name }))
        .transpose()?;
    normalize_optional(name, value)
}

fn normalize_optional(
    name: &'static str,
    value: Option<&str>,
) -> Result<Option<String>, EnvConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        Err(EnvConfigError::Empty { name })
    } else {
        Ok(Some(value.to_string()))
    }
}

pub fn string_or(name: &'static str, default: &str) -> Result<String, EnvConfigError> {
    Ok(optional_nonempty(name)?.unwrap_or_else(|| default.to_string()))
}

pub fn path_or(name: &'static str, default: &str) -> Result<PathBuf, EnvConfigError> {
    string_or(name, default).map(PathBuf::from)
}

pub fn optional_path(name: &'static str) -> Result<Option<PathBuf>, EnvConfigError> {
    optional_nonempty(name).map(|value| value.map(PathBuf::from))
}

pub fn port_or(name: &'static str, default: u16) -> Result<u16, EnvConfigError> {
    let value = optional_nonempty(name)?;
    parse_port(name, value.as_deref(), default)
}

fn parse_port(
    name: &'static str,
    value: Option<&str>,
    default: u16,
) -> Result<u16, EnvConfigError> {
    let Some(value) = normalize_optional(name, value)? else {
        return Ok(default);
    };
    value
        .parse::<u16>()
        .map_err(|error| EnvConfigError::InvalidPort {
            name,
            value,
            message: error.to_string(),
        })
}

pub fn bool_or(name: &'static str, default: impl FnOnce() -> bool) -> Result<bool, EnvConfigError> {
    let Some(value) = optional_nonempty(name)? else {
        return Ok(default());
    };
    parse_bool(name, &value)
}

/// Enable an optional command-backed feature only when its executable is
/// available. An absent setting follows command availability; explicit false
/// disables it; explicit true fails startup when the command cannot run.
pub fn enabled_when_available(name: &'static str, command: &str) -> Result<bool, EnvConfigError> {
    let configured = optional_nonempty(name)?;
    resolve_enabled_when_available(
        name,
        command,
        configured.as_deref(),
        command_available(command),
    )
}

fn resolve_enabled_when_available(
    name: &'static str,
    command: &str,
    configured: Option<&str>,
    available: bool,
) -> Result<bool, EnvConfigError> {
    let Some(value) = normalize_optional(name, configured)? else {
        return Ok(available);
    };
    let enabled = parse_bool(name, &value)?;
    if enabled && !available {
        return Err(EnvConfigError::UnavailableCommand {
            name,
            command: command.to_string(),
        });
    }
    Ok(enabled)
}

/// Report whether `program` resolves to a regular executable file. Explicit
/// paths are checked directly; bare command names are searched through PATH.
pub fn command_available(program: &str) -> bool {
    let candidate_is_executable = |path: &Path| {
        let Ok(metadata) = std::fs::metadata(path) else {
            return false;
        };
        if !metadata.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    };
    let path = Path::new(program);
    if path.components().count() > 1 {
        return candidate_is_executable(path);
    }
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .map(|directory| directory.join(program))
                .any(|candidate| candidate_is_executable(&candidate))
        })
        .unwrap_or(false)
}

fn parse_bool(name: &'static str, value: &str) -> Result<bool, EnvConfigError> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" => Ok(true),
        "0" | "false" | "off" | "no" => Ok(false),
        _ => Err(EnvConfigError::InvalidBoolean {
            name,
            value: value.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boolean_parser_is_explicit() {
        for value in ["1", "true", "on", "yes", "TRUE"] {
            assert!(parse_bool("TEST", value).expect("true value"));
        }
        for value in ["0", "false", "off", "no", "FALSE"] {
            assert!(!parse_bool("TEST", value).expect("false value"));
        }
        assert!(parse_bool("TEST", "maybe").is_err());
        assert!(parse_bool("TEST", "").is_err());
    }

    #[test]
    fn present_empty_values_are_not_defaults() {
        assert_eq!(normalize_optional("TEST", None).expect("absent"), None);
        assert!(normalize_optional("TEST", Some("")).is_err());
        assert!(normalize_optional("TEST", Some("   ")).is_err());
        assert_eq!(
            normalize_optional("TEST", Some(" value ")).expect("trimmed"),
            Some("value".into())
        );
    }

    #[test]
    fn port_parser_rejects_present_invalid_values() {
        assert_eq!(parse_port("PORT", None, 8090).expect("default"), 8090);
        assert_eq!(
            parse_port("PORT", Some(" 8091 "), 8090).expect("valid port"),
            8091
        );
        for value in ["", "   ", "not-a-port", "65536", "-1"] {
            assert!(
                parse_port("PORT", Some(value), 8090).is_err(),
                "accepted {value:?}"
            );
        }
    }

    #[test]
    fn command_backed_feature_resolution_is_fail_closed() {
        assert!(resolve_enabled_when_available("AUDIO", "speak", None, true)
            .expect("available default"));
        assert!(
            !resolve_enabled_when_available("AUDIO", "speak", None, false)
                .expect("unavailable default")
        );
        assert!(
            !resolve_enabled_when_available("AUDIO", "speak", Some("false"), true)
                .expect("explicit disable")
        );
        assert!(
            !resolve_enabled_when_available("AUDIO", "speak", Some("false"), false)
                .expect("explicit disable does not require command")
        );
        assert!(
            resolve_enabled_when_available("AUDIO", "speak", Some("true"), true)
                .expect("explicit enable with available command")
        );
        assert!(matches!(
            resolve_enabled_when_available("AUDIO", "speak", Some("true"), false),
            Err(EnvConfigError::UnavailableCommand {
                name: "AUDIO",
                command
            }) if command == "speak"
        ));
        assert!(resolve_enabled_when_available("AUDIO", "speak", Some("maybe"), true).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn command_availability_requires_an_executable_regular_file() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "ag-ui-command-availability-{}-{}",
            std::process::id(),
            line!()
        ));
        let command = root.join("command");
        std::fs::create_dir_all(&root).expect("create command fixture directory");
        std::fs::write(&command, b"#!/bin/sh\nexit 0\n").expect("write command fixture");

        let mut permissions = std::fs::metadata(&command)
            .expect("command fixture metadata")
            .permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&command, permissions).expect("remove executable bit");
        assert!(!command_available(
            command.to_str().expect("Unicode temp path")
        ));

        let mut permissions = std::fs::metadata(&command)
            .expect("command fixture metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&command, permissions).expect("add executable bit");
        assert!(command_available(
            command.to_str().expect("Unicode temp path")
        ));
        assert!(!command_available(
            root.to_str().expect("Unicode temp path")
        ));

        std::fs::remove_dir_all(root).expect("remove command fixture");
    }
}
