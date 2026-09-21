//! E1, and E2-E5, for the JavaScript/TypeScript side: `package.json`'s
//! `bin`/`main`/`scripts.start` for binaries, and regex over
//! comment-stripped source lines for everything else.

use std::path::Path;

use regex::Regex;
use serde_json::Value;

use crate::util::{depth_at, line_at, line_number_at, sha256_hex, strip_line_comment, trimmed_snippet};
use crate::walk::Walked;
use crate::{Evidence, Lane, LaneKind, ScanError};

/// E1 (JS): `package.json` `bin` (string or object form), `main`, and
/// `scripts.start`, each naming a runnable file — each becomes its own
/// `Lane::Binary`. Position is recovered by searching the raw manifest text
/// for the value serde_json found, since `serde_json::Value` itself carries
/// no source location.
pub fn binary_lanes(_root: &Path, walked: &Walked) -> Result<Vec<Lane>, ScanError> {
    let mut lanes = Vec::new();

    for manifest in walked
        .files
        .iter()
        .filter(|f| f.rel_path.file_name().and_then(|n| n.to_str()) == Some("package.json"))
    {
        let Ok(value) = serde_json::from_str::<Value>(&manifest.contents) else {
            continue; // not valid JSON; nothing to safely extract
        };
        let sha = sha256_hex(manifest.contents.as_bytes());
        let package_name = value
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string);

        let mut push_binary = |label: String, detail: String, target: &str| {
            if let Some((line, snippet)) = find_value_line(&manifest.contents, target) {
                lanes.push(Lane {
                    kind: LaneKind::Binary,
                    label,
                    evidence: Evidence {
                        path: manifest.rel_path.clone(),
                        line,
                        snippet,
                        sha256: sha.clone(),
                    },
                    package: package_name.clone(),
                    detail,
                });
            }
        };

        match value.get("bin") {
            Some(Value::String(path)) => {
                let label = package_name.clone().unwrap_or_else(|| path.clone());
                push_binary(
                    label,
                    format!("package.json \"bin\": \"{path}\""),
                    path,
                );
            }
            Some(Value::Object(map)) => {
                for (name, v) in map {
                    if let Some(path) = v.as_str() {
                        push_binary(
                            name.clone(),
                            format!("package.json \"bin\".\"{name}\": \"{path}\""),
                            path,
                        );
                    }
                }
            }
            _ => {}
        }

        if let Some(main) = value.get("main").and_then(Value::as_str) {
            push_binary(
                package_name.clone().unwrap_or_else(|| main.to_string()),
                format!("package.json \"main\": \"{main}\""),
                main,
            );
        }

        if let Some(start) = value
            .get("scripts")
            .and_then(|s| s.get("start"))
            .and_then(Value::as_str)
        {
            push_binary(
                package_name.clone().unwrap_or_else(|| "start".to_string()),
                format!("package.json \"scripts\".\"start\": \"{start}\""),
                start,
            );
        }
    }

    Ok(lanes)
}

/// Finds the quoted occurrence of `value` in `text` and returns its 1-based
/// line number and trimmed line text. Used to recover evidence for a
/// `serde_json`-parsed field, which otherwise carries no position.
fn find_value_line(text: &str, value: &str) -> Option<(u32, String)> {
    let quoted = format!("\"{value}\"");
    let byte_offset = text.find(&quoted)?;
    let line = line_number_at(text, byte_offset);
    Some((line, trimmed_snippet(line_at(text, byte_offset))))
}

/// E2-E5 for a single JS/TS source file.
pub fn extract_js(root: &Path, rel_path: &Path, contents: &str) -> Result<Vec<Lane>, ScanError> {
    let _ = root;
    let mut lanes = Vec::new();

    let listener_re =
        Regex::new(r"\.listen\s*\(|createServer\s*\(|Bun\.serve\s*\(|Deno\.serve\s*\(").unwrap();
    let spawn_re = Regex::new(
        r"child_process|\bspawn\s*\(|\bexec\s*\(|\bexecFile\s*\(|\bfork\s*\(|Bun\.spawn\s*\(|new\s+Worker\s*\(",
    )
    .unwrap();
    // Three quote alternatives spelled out rather than one pattern with a
    // backreference: the `regex` crate deliberately doesn't support
    // backreferences (they break its linear-time guarantee), so `\1` here
    // would fail to compile.
    let fetch_re = Regex::new(
        r#"\bfetch\s*\(\s*(?:"(?P<dq>[^"]*)"|'(?P<sq>[^']*)'|`(?P<bq>[^`]*)`|(?P<var>[A-Za-z_$][\w$]*))"#,
    )
    .unwrap();
    let outbound_re = Regex::new(r"\bWebSocket\s*\(|http\.request\s*\(|\baxios\b").unwrap();
    let interval_re = Regex::new(r"\bsetInterval\s*\(").unwrap();
    let background_misc_re = Regex::new(r"\bcron\b|\bqueue\.process\s*\(").unwrap();

    let sha = sha256_hex(contents.as_bytes());
    let mut offset = 0usize;

    for (line_no, raw_line) in (1_u32..).zip(contents.split_inclusive('\n')) {
        let line_start = offset;
        offset += raw_line.len();

        let line = raw_line.trim_end_matches(['\n', '\r']);
        let stripped = strip_line_comment(line);
        if stripped.trim().is_empty() {
            continue;
        }

        let mut push = |kind: LaneKind, detail: &str| {
            lanes.push(Lane {
                kind,
                label: rel_path.display().to_string(),
                evidence: Evidence {
                    path: rel_path.to_path_buf(),
                    line: line_no,
                    snippet: trimmed_snippet(stripped),
                    sha256: sha.clone(),
                },
                package: None,
                detail: detail.to_string(),
            });
        };

        if listener_re.is_match(stripped) {
            push(LaneKind::Listener, ".listen()/createServer()/Bun.serve()/Deno.serve()");
        }
        if spawn_re.is_match(stripped) {
            push(LaneKind::Spawn, "child_process spawn/exec/execFile/fork or Worker");
        }
        if outbound_re.is_match(stripped) {
            push(LaneKind::Outbound, "WebSocket/http.request/axios call");
        }
        if let Some(caps) = fetch_re.captures(stripped) {
            let literal = caps
                .name("dq")
                .or_else(|| caps.name("sq"))
                .or_else(|| caps.name("bq"));
            let is_relative = literal
                .map(|lit| lit.as_str().starts_with('/') || lit.as_str().starts_with('.'))
                .unwrap_or(false);
            if !is_relative {
                push(LaneKind::Outbound, "fetch() to an absolute URL or a variable");
            }
        }
        if background_misc_re.is_match(stripped) {
            push(LaneKind::Background, "cron / queue.process() background task");
        }
        if let Some(m) = interval_re.find(stripped) {
            let abs_offset = line_start + m.start();
            if depth_at(contents, abs_offset) == 0 {
                push(LaneKind::Background, "setInterval() at module top level");
            }
        }
    }

    Ok(lanes)
}
