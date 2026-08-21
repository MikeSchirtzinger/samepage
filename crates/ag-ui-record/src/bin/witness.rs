use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use ag_ui_record::{validate, Record, RECORD_VERSION};
use serde::Serialize;

const USAGE: &str = "usage: witness validate <path/to/record.json> [--json]";

#[derive(Serialize)]
struct JsonViolation<'a> {
    check: &'static str,
    message: &'a str,
}

#[derive(Serialize)]
struct JsonOutput<'a> {
    record_version: &'a str,
    ok: bool,
    violations: Vec<JsonViolation<'a>>,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("witness: {message}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let (path, json) = parse_args()?;
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let record = Record::from_json(&text)
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;

    if record.record_version != RECORD_VERSION {
        return Err(format!(
            "unsupported record_version {:?}; expected {RECORD_VERSION:?}",
            record.record_version
        ));
    }

    let violations = validate(&record);
    let ok = violations.is_empty();

    if json {
        let output = JsonOutput {
            record_version: &record.record_version,
            ok,
            violations: violations
                .iter()
                .map(|violation| JsonViolation {
                    check: violation.check.slug(),
                    message: &violation.message,
                })
                .collect(),
        };
        write_json(&output)?;
    } else {
        for violation in &violations {
            eprintln!("{}: {}", violation.check.slug(), violation.message);
        }
    }

    Ok(ExitCode::from(u8::from(!ok)))
}

fn parse_args() -> Result<(PathBuf, bool), String> {
    let mut args = std::env::args_os().skip(1);
    let command = args.next().ok_or_else(|| USAGE.to_string())?;
    if command != OsStr::new("validate") {
        return Err(USAGE.to_string());
    }

    let path = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| USAGE.to_string())?;
    let json = match args.next() {
        None => false,
        Some(flag) if flag == OsStr::new("--json") && args.next().is_none() => true,
        Some(_) => return Err(USAGE.to_string()),
    };

    Ok((path, json))
}

fn write_json(output: &JsonOutput<'_>) -> Result<(), String> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    serde_json::to_writer(&mut stdout, output)
        .map_err(|error| format!("cannot write JSON output: {error}"))?;
    writeln!(stdout).map_err(|error| format!("cannot finish JSON output: {error}"))
}
