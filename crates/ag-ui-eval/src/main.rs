use std::fs;
use std::path::{Path, PathBuf};

use ag_ui_eval::model::{Layer, SCHEMA_VERSION};
use serde::Serialize;
use serde_json::json;

enum Cli {
    Run {
        layer: Layer,
        suite: PathBuf,
        project_root: PathBuf,
        output: Option<PathBuf>,
    },
    ValidateRunScoring {
        contract: PathBuf,
        output: Option<PathBuf>,
    },
    Help,
}

fn main() {
    let cli = match parse_cli() {
        Ok(cli) => cli,
        Err(error) => exit_with_cli_error(error),
    };

    match cli {
        Cli::Run {
            layer,
            suite,
            project_root,
            output,
        } => {
            let receipt = ag_ui_eval::run_suite(layer, &suite, &project_root);
            let exit_code = receipt.status.exit_code();
            emit_or_exit(&receipt, output.as_deref(), exit_code);
        }
        Cli::ValidateRunScoring { contract, output } => {
            let receipt = ag_ui_eval::validate_contract(&contract);
            let exit_code = receipt.status.exit_code();
            emit_or_exit(&receipt, output.as_deref(), exit_code);
        }
        Cli::Help => {
            println!("{}", usage());
        }
    }
}

fn parse_cli() -> Result<Cli, String> {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        return Err(format!("missing command\n\n{}", usage()));
    };
    match command.as_str() {
        "run" => {
            let mut layer = None;
            let mut suite = None;
            let mut project_root = None;
            let mut output = None;
            while let Some(flag) = args.next() {
                match flag.as_str() {
                    "--layer" => {
                        let value = next_value(&mut args, "--layer")?;
                        layer = Some(value.parse::<Layer>()?);
                    }
                    "--suite" => suite = Some(PathBuf::from(next_value(&mut args, "--suite")?)),
                    "--project-root" => {
                        project_root =
                            Some(PathBuf::from(next_value(&mut args, "--project-root")?));
                    }
                    "--output" => {
                        output = Some(PathBuf::from(next_value(&mut args, "--output")?));
                    }
                    "--help" | "-h" => return Ok(Cli::Help),
                    _ => return Err(format!("unknown run argument {flag:?}\n\n{}", usage())),
                }
            }
            let layer = layer.ok_or_else(|| "run requires --layer".to_string())?;
            if layer == Layer::RunScoring {
                return Err(
                    "run_scoring is a post-hoc contract, not an executable scenario suite; use validate-run-scoring"
                        .to_string(),
                );
            }
            let suite = suite.ok_or_else(|| "run requires --suite".to_string())?;
            let project_root = match project_root {
                Some(path) => path,
                None => std::env::current_dir()
                    .map_err(|error| format!("could not resolve current directory: {error}"))?,
            };
            Ok(Cli::Run {
                layer,
                suite,
                project_root,
                output,
            })
        }
        "validate-run-scoring" => {
            let mut contract = None;
            let mut output = None;
            while let Some(flag) = args.next() {
                match flag.as_str() {
                    "--contract" => {
                        contract = Some(PathBuf::from(next_value(&mut args, "--contract")?));
                    }
                    "--output" => {
                        output = Some(PathBuf::from(next_value(&mut args, "--output")?));
                    }
                    "--help" | "-h" => return Ok(Cli::Help),
                    _ => {
                        return Err(format!(
                            "unknown validate-run-scoring argument {flag:?}\n\n{}",
                            usage()
                        ));
                    }
                }
            }
            Ok(Cli::ValidateRunScoring {
                contract: contract
                    .ok_or_else(|| "validate-run-scoring requires --contract".to_string())?,
                output,
            })
        }
        "--help" | "-h" | "help" => Ok(Cli::Help),
        _ => Err(format!("unknown command {command:?}\n\n{}", usage())),
    }
}

fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn usage() -> &'static str {
    "ag-ui-eval

USAGE:
  ag-ui-eval run --layer <conformance|agent_eval> --suite <path> [--project-root <path>] [--output <path>]
  ag-ui-eval validate-run-scoring --contract <path> [--output <path>]

EXIT CODES:
  0  every executed scenario passed
  1  a scenario or scorer failed
  2  missing, empty, invalid, or unreadable configuration
  3  one or more agent evals were skipped; skip is not pass"
}

fn emit_or_exit<T: Serialize>(value: &T, output: Option<&Path>, exit_code: i32) {
    let serialized = match serde_json::to_string_pretty(value) {
        Ok(serialized) => serialized,
        Err(error) => exit_with_cli_error(format!("could not serialize receipt: {error}")),
    };
    if let Some(path) = output {
        if let Err(error) = write_output(path, &serialized) {
            exit_with_cli_error(error);
        }
    }
    println!("{serialized}");
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

fn write_output(path: &Path, serialized: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "could not create receipt directory {}: {error}",
                    parent.display()
                )
            })?;
        }
    }
    fs::write(path, format!("{serialized}\n"))
        .map_err(|error| format!("could not write receipt {}: {error}", path.display()))
}

fn exit_with_cli_error(error: String) -> ! {
    let receipt = json!({
        "schema_version": SCHEMA_VERSION,
        "status": "configuration_error",
        "errors": [error],
    });
    match serde_json::to_string_pretty(&receipt) {
        Ok(serialized) => println!("{serialized}"),
        Err(serialization_error) => {
            eprintln!("configuration error: {serialization_error}");
        }
    }
    std::process::exit(2);
}
