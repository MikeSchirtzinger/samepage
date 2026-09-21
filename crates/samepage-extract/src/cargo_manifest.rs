//! E1 for Rust: a Cargo package is a `Lane::Binary` when it declares an
//! explicit `[[bin]]` target, has an implicit default binary via
//! `src/main.rs`, or has one or more `src/bin/*.rs` files.
//!
//! This does not resolve `[workspace] members` lists to decide which
//! manifests to look at. It looks at every `Cargo.toml` the walk found,
//! independently, and checks the filesystem for `src/main.rs` /
//! `src/bin/*.rs` directly. A manifest on disk that a workspace's members
//! array forgot to list is exactly the kind of thing this crate exists to
//! surface, not silently trust its way past.
//!
//! No TOML parser is used (none is on this pass's dependency list): Cargo
//! manifests are formulaic enough that a small line-oriented section
//! scanner finds `[package]`, `[[bin]]`, and their `name = "..."` values
//! reliably, without pulling in a full parser for a few known keys.

use std::path::Path;

use regex::Regex;

use crate::util::{line_at, line_number_at, sha256_hex, trimmed_snippet};
use crate::walk::Walked;
use crate::{Evidence, Lane, LaneKind, ScanError};

struct TomlSection {
    name: String,
    is_array_table: bool,
    body_start: usize,
    body_end: usize,
}

/// Splits a TOML file into `[section]` / `[[array.table]]` bodies by
/// scanning for header lines; each section's body runs from just after its
/// header to just before the next header (or EOF).
fn toml_sections(header_re: &Regex, text: &str) -> Vec<TomlSection> {
    let headers: Vec<(usize, usize, String, bool)> = header_re
        .captures_iter(text)
        .map(|c| {
            let whole = c.get(0).unwrap();
            (whole.start(), whole.end(), c[2].to_string(), &c[1] == "[[")
        })
        .collect();

    headers
        .iter()
        .enumerate()
        .map(|(i, (_, end, name, is_array))| TomlSection {
            name: name.clone(),
            is_array_table: *is_array,
            body_start: *end,
            body_end: headers.get(i + 1).map(|h| h.0).unwrap_or(text.len()),
        })
        .collect()
}

pub fn binary_lanes(root: &Path, walked: &Walked) -> Result<Vec<Lane>, ScanError> {
    let mut lanes = Vec::new();
    let name_re = Regex::new(r#"(?m)^[ \t]*name[ \t]*=[ \t]*"([^"]*)""#).unwrap();
    let header_re =
        Regex::new(r"(?m)^[ \t]*(\[\[?)[ \t]*([A-Za-z0-9_.-]+)[ \t]*(\]\]?)[ \t]*$").unwrap();

    for manifest in walked
        .files
        .iter()
        .filter(|f| f.rel_path.file_name().and_then(|n| n.to_str()) == Some("Cargo.toml"))
    {
        let sections = toml_sections(&header_re, &manifest.contents);
        let Some(package_section) = sections
            .iter()
            .find(|s| !s.is_array_table && s.name == "package")
        else {
            continue; // a workspace-only manifest; no package of its own
        };

        let manifest_sha = sha256_hex(manifest.contents.as_bytes());
        let package_dir = manifest
            .rel_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let package_body =
            &manifest.contents[package_section.body_start..package_section.body_end];
        let package_name = name_re.captures(package_body).map(|c| c[1].to_string());

        let bin_sections: Vec<&TomlSection> = sections
            .iter()
            .filter(|s| s.is_array_table && s.name == "bin")
            .collect();

        if bin_sections.is_empty() {
            if root.join(&package_dir).join("src/main.rs").is_file() {
                if let Some(name_match) = name_re.captures(package_body) {
                    let abs_offset =
                        package_section.body_start + name_match.get(0).unwrap().start();
                    lanes.push(Lane {
                        kind: LaneKind::Binary,
                        label: name_match[1].to_string(),
                        evidence: Evidence {
                            path: manifest.rel_path.clone(),
                            line: line_number_at(&manifest.contents, abs_offset),
                            snippet: trimmed_snippet(line_at(&manifest.contents, abs_offset)),
                            sha256: manifest_sha.clone(),
                        },
                        package: package_name.clone(),
                        detail: "package has src/main.rs (implicit default binary)".to_string(),
                    });
                }
            }
        } else {
            for bin in &bin_sections {
                let body = &manifest.contents[bin.body_start..bin.body_end];
                let Some(name_match) = name_re.captures(body) else {
                    continue;
                };
                let abs_offset = bin.body_start + name_match.get(0).unwrap().start();
                lanes.push(Lane {
                    kind: LaneKind::Binary,
                    label: name_match[1].to_string(),
                    evidence: Evidence {
                        path: manifest.rel_path.clone(),
                        line: line_number_at(&manifest.contents, abs_offset),
                        snippet: trimmed_snippet(line_at(&manifest.contents, abs_offset)),
                        sha256: manifest_sha.clone(),
                    },
                    package: package_name.clone(),
                    detail: "explicit [[bin]] target in Cargo.toml".to_string(),
                });
            }
        }

        // src/bin/*.rs: implicit extra binaries, independent of [[bin]].
        let bin_dir = root.join(&package_dir).join("src/bin");
        if let Ok(read_dir) = std::fs::read_dir(&bin_dir) {
            let mut bin_files: Vec<_> = read_dir
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("rs"))
                .collect();
            bin_files.sort();
            for bin_file in bin_files {
                let rel = bin_file.strip_prefix(root).unwrap_or(&bin_file).to_path_buf();
                let Some(walked_file) = walked.get(&rel) else {
                    continue; // filtered out by the walk (too large, binary, tests/)
                };
                let name = bin_file
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                let first_line = walked_file.contents.lines().next().unwrap_or("");
                lanes.push(Lane {
                    kind: LaneKind::Binary,
                    label: name,
                    evidence: Evidence {
                        path: rel,
                        line: 1,
                        snippet: trimmed_snippet(first_line),
                        sha256: sha256_hex(walked_file.contents.as_bytes()),
                    },
                    package: package_name.clone(),
                    detail: "src/bin/*.rs (implicit binary target)".to_string(),
                });
            }
        }
    }

    Ok(lanes)
}
