//! `cargo run -p samepage-extract -- <path>` — scans `<path>` and prints
//! the resulting [`samepage_extract::Report`] as pretty JSON on stdout.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let Some(root_arg) = args.next() else {
        eprintln!("usage: samepage-extract <path-to-project-root>");
        return ExitCode::FAILURE;
    };
    let root = PathBuf::from(root_arg);

    match samepage_extract::scan(&root) {
        Ok(report) => match serde_json::to_string_pretty(&report) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("failed to serialize report: {err}");
                ExitCode::FAILURE
            }
        },
        Err(err) => {
            eprintln!("scan failed: {err}");
            ExitCode::FAILURE
        }
    }
}
