//! E2-E5 for Rust source files: listeners, spawned processes, outbound
//! calls, and background tasks, found by regex over comment-stripped lines
//! plus a small amount of brace-depth analysis for the background-task
//! heuristic (see [`crate::LaneKind::Background`] for its limits).

use std::path::Path;

use regex::Regex;

use crate::util::{
    cfg_test_ranges, fn_spans, local_depth_at, sha256_hex, blank_string_literals, strip_line_comment, trimmed_snippet,
};
use crate::{Evidence, Lane, LaneKind, ScanError};

pub fn extract(root: &Path, rel_path: &Path, contents: &str) -> Result<Vec<Lane>, ScanError> {
    let _ = root; // evidence paths are already relative; kept for signature symmetry
    let mut lanes = Vec::new();

    let listener_re = Regex::new(
        r"TcpListener::bind|UdpSocket::bind|axum::Server::bind|\bserve\s*\(|\b\w*(?i:listener|socket)\w*\s*\.bind\s*\(",
    )
    .unwrap();
    // A constructor or a call. `cmd: &mut tokio::process::Command` in a
    // signature is a type, not a child process.
    let spawn_re = Regex::new(
        r"Command::new\s*\(|process::Command::new\s*\(|\.spawn\s*\(\s*\)",
    )
    .unwrap();
    let background_re =
        Regex::new(r"tokio::spawn\s*\(|std::thread::spawn\s*\(|\bthread::spawn\s*\(").unwrap();

    // Only a line that names an HTTP client on its own is outbound evidence.
    // A file-wide rule ("Client::new() somewhere, so every .get( is a request")
    // flagged HashMap::get and axum route registrations; a lane that might be
    // a map lookup is not a lane a person can trust.
    // `reqwest::Url` in a signature is a type, not a call.
    let reqwest_re = Regex::new(
        r"reqwest::(Client::(new|builder)\s*\(|ClientBuilder::new\s*\(|blocking::|get\s*\(|post\s*\()",
    )
    .unwrap();
    let http_client_re = Regex::new(r"\bClient::builder\s*\(\)|\bClient::new\s*\(\)").unwrap();

    let test_ranges = cfg_test_ranges(contents);
    let spans = fn_spans(contents);
    let main_span = spans
        .iter()
        .find(|s| s.name == "main" || s.is_tokio_main);
    let entry_keyword_re =
        Regex::new(r"(?i)serve|run|start|boot|worker|loop|tick").unwrap();

    let sha = sha256_hex(contents.as_bytes());
    let mut offset = 0usize;

    for (line_no, raw_line) in (1_u32..).zip(contents.split_inclusive('\n')) {
        let line_start = offset;
        offset += raw_line.len();

        if test_ranges
            .iter()
            .any(|&(s, e)| line_start >= s && line_start < e)
        {
            continue; // inside a #[cfg(test)] item; not a real lane
        }

        let line = raw_line.trim_end_matches(['\n', '\r']);
        let with_strings = strip_line_comment(line);
        if with_strings.trim().is_empty() {
            continue;
        }
        // Patterns are matched with string contents blanked; the snippet keeps
        // the real text.
        let blanked = blank_string_literals(with_strings);
        let stripped: &str = &blanked;
        // An import names a type; it does not bind, spawn, or connect. Without
        // this, `use std::process::Command;` reads as a sidecar.
        let head = stripped.trim_start();
        if head.starts_with("use ") || head.starts_with("pub use ") || head.starts_with("extern crate ") {
            continue;
        }

        let push = |lanes: &mut Vec<Lane>, kind: LaneKind, detail: &str| {
            lanes.push(Lane {
                kind,
                label: rel_path.display().to_string(),
                evidence: Evidence {
                    path: rel_path.to_path_buf(),
                    line: line_no,
                    snippet: trimmed_snippet(with_strings),
                    sha256: sha.clone(),
                },
                package: None,
                detail: detail.to_string(),
            });
        };

        if listener_re.is_match(stripped) {
            push(&mut lanes, LaneKind::Listener, "socket bind or serve() call");
        }
        if spawn_re.is_match(stripped) {
            // `Command::new("node")` on one line and its `.spawn()` a few
            // lines down are one child process, not two. A `.spawn()` that
            // follows a construction within the same short statement is the
            // same lane; a bare `.spawn()` on a command built elsewhere still
            // counts on its own.
            let continues_a_construction = stripped.trim_start().starts_with('.')
                && lanes.iter().rev().take(1).any(|lane| {
                    lane.kind == LaneKind::Spawn && line_no.saturating_sub(lane.evidence.line) <= 6
                });
            if !continues_a_construction {
                push(
                    &mut lanes,
                    LaneKind::Spawn,
                    "std::process::Command construction or .spawn()",
                );
            }
        }
        if http_client_re.is_match(stripped) && !stripped.contains("reqwest::") {
            push(&mut lanes, LaneKind::Outbound, "HTTP client constructed");
        }
        if reqwest_re.is_match(stripped) {
            push(&mut lanes, LaneKind::Outbound, "reqwest:: client or call");
        }
        if stripped.contains("TcpStream::connect") {
            push(&mut lanes, LaneKind::Outbound, "raw TcpStream::connect");
        }
        if stripped.contains("ureq::") {
            push(&mut lanes, LaneKind::Outbound, "ureq:: call");
        }
        if stripped.contains("hyper::Client") {
            push(&mut lanes, LaneKind::Outbound, "hyper::Client construction");
        }

        for m in background_re.find_iter(stripped) {
            let abs_offset = line_start + m.start();
            let Some(span) = spans
                .iter()
                .filter(|s| s.body_start < abs_offset && abs_offset < s.body_end)
                .max_by_key(|s| s.body_start)
            else {
                continue; // spawn call outside any fn body this crate could span
            };
            if local_depth_at(contents, span.body_start, abs_offset) != 0 {
                continue; // nested inside an inner block, not the fn's own top level
            }
            let is_entry = span.name == "main"
                || span.is_tokio_main
                || (entry_keyword_re.is_match(&span.name)
                    && main_span
                        .map(|m| contents[m.body_start..m.body_end].contains(&format!("{}(", span.name)))
                        .unwrap_or(false));
            if !is_entry {
                continue;
            }
            lanes.push(Lane {
                kind: LaneKind::Background,
                label: rel_path.display().to_string(),
                evidence: Evidence {
                    path: rel_path.to_path_buf(),
                    line: line_no,
                    snippet: trimmed_snippet(with_strings),
                    sha256: sha.clone(),
                },
                package: None,
                detail: format!(
                    "tokio::spawn/thread::spawn at the top level of `{}`, an entry point",
                    span.name
                ),
            });
        }
    }

    Ok(lanes)
}
