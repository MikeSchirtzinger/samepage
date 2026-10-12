//! Native repository evidence and per-participant read cursors. No filesystem
//! or Git implementation is linked into the review WASM artifact.
use crate::diagram_sources::DiagramSources;
use samepage_capability::{CapError, Capability, Change, View};
use samepage_doc::host::Verifier;
use samepage_review::{
    report::{self, Facts},
    string, Review,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

pub struct ReviewHost {
    root: PathBuf,
    sources: DiagramSources,
    cursors: Mutex<BTreeMap<String, Value>>,
}
impl ReviewHost {
    pub fn open(root: &Path) -> Result<Self, String> {
        Ok(Self {
            root: root.canonicalize().map_err(|e| e.to_string())?,
            sources: DiagramSources::open(root)?,
            cursors: Mutex::new(BTreeMap::new()),
        })
    }
    fn source(&self, path: &str, lines: &str, revision: &str) -> Result<String, CapError> {
        if revision.is_empty() {
            return self
                .sources
                .read(path, if lines.is_empty() { "1" } else { lines });
        }
        samepage_review::basis_and_source("verified", path, lines, revision)?;
        // Apply the same private-path policy to archived reads, even when a
        // tracked file no longer exists in the working tree.
        let lower = path.to_ascii_lowercase();
        if lower.split('/').any(|p| {
            [
                ".git",
                ".local",
                ".claude",
                ".ssh",
                "target",
                "node_modules",
                "pkg",
            ]
            .contains(&p)
                || p.starts_with(".env")
        }) || ["secret", "credential", "password", "token", "auth.json"]
            .iter()
            .any(|s| lower.contains(s))
            || Path::new(path).extension().is_some_and(|e| {
                [
                    "pem", "key", "p12", "pfx", "crt", "der", "wasm", "png", "jpg", "jpeg", "gif",
                    "webp", "ico", "pdf", "zip", "gz", "tar", "bin", "so", "dylib", "dll", "mp4",
                    "mov", "woff", "woff2", "ttf",
                ]
                .contains(&e.to_string_lossy().to_ascii_lowercase().as_str())
            })
        {
            return Err(CapError::new(
                "source_invalid",
                "private or binary source path",
            ));
        }
        let run = |args: &[&str]| -> Result<Vec<u8>, CapError> {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&self.root)
                .output()
                .map_err(|e| CapError::new("source_invalid", e.to_string()))?;
            if !output.status.success() {
                return Err(CapError::new(
                    "source_invalid",
                    String::from_utf8_lossy(&output.stderr).trim(),
                ));
            }
            Ok(output.stdout)
        };
        let commit = format!("{revision}^{{commit}}");
        run(&["rev-parse", "--verify", &commit])?;
        let tree = run(&["--literal-pathspecs", "ls-tree", "-z", revision, "--", path])?;
        let listing = std::str::from_utf8(&tree)
            .map_err(|_| CapError::new("source_invalid", "invalid Git tree encoding"))?;
        let entries: Vec<_> = listing.split('\0').filter(|s| !s.is_empty()).collect();
        let entry = if entries.len() == 1 {
            entries[0]
        } else {
            return Err(CapError::new(
                "source_invalid",
                "revision path must name one file",
            ));
        };
        let (header, name) = entry
            .split_once('\t')
            .ok_or_else(|| CapError::new("source_invalid", "invalid Git tree entry"))?;
        let fields: Vec<_> = header.split_whitespace().collect();
        if name != path
            || fields.len() != 3
            || !matches!(fields[0], "100644" | "100755")
            || fields[1] != "blob"
            || fields[2].len() != 40
            || !fields[2].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(CapError::new(
                "source_invalid",
                "revision path must be a regular file",
            ));
        }
        // ls-tree applies the selected project's cwd. The exact blob OID
        // avoids cat-file's repository-root path interpretation.
        let object = fields[2];
        let size = run(&["cat-file", "-s", object])?;
        if String::from_utf8_lossy(&size)
            .trim()
            .parse::<usize>()
            .map_err(|_| CapError::new("source_invalid", "invalid Git blob size"))?
            > 2 * 1024 * 1024
        {
            return Err(CapError::new("source_invalid", "source file exceeds 2 MiB"));
        }
        let bytes = run(&["cat-file", "blob", object])?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| CapError::new("source_invalid", "source is not UTF-8"))?;
        if text.contains('\0') {
            return Err(CapError::new("source_invalid", "binary source"));
        }
        let (start, end) = lines.split_once('-').unwrap_or((lines, lines));
        let start = start
            .parse::<usize>()
            .map_err(|_| CapError::new("source_invalid", "invalid range"))?;
        let end = end
            .parse::<usize>()
            .map_err(|_| CapError::new("source_invalid", "invalid range"))?;
        let selected: Vec<_> = text
            .split_inclusive('\n')
            .skip(start - 1)
            .take(end - start + 1)
            .collect();
        if selected.len() != end - start + 1 {
            return Err(CapError::new(
                "source_invalid",
                format!(
                    "{path} has {} lines at {revision}, so {lines} ends past it",
                    text.split_inclusive('\n').count()
                ),
            ));
        }
        let excerpt = selected.join("");
        if excerpt.chars().count() > 24000 {
            return Err(CapError::new(
                "source_invalid",
                "source excerpt exceeds 24000 characters",
            ));
        }
        Ok(excerpt)
    }
    pub fn inspect(&self, view: &View) -> Result<Value, String> {
        let mut facts = Facts {
            repository: self.root.display().to_string(),
            project: project_facts(&self.root),
            ..Default::default()
        };
        for node in view.records().filter(|r| r.kind == "node") {
            let mut references = std::collections::BTreeSet::new();
            if !string(node, "path").is_empty() {
                references.insert((string(node, "path"), string(node, "lines"), ""));
            }
            for claim in samepage_review::claims_of(view, &node.id)
                .into_iter()
                .filter(|r| !samepage_review::flag(r, "withdrawn"))
            {
                if !string(claim, "path").is_empty() {
                    references.insert((
                        string(claim, "path"),
                        string(claim, "lines"),
                        string(claim, "revision"),
                    ));
                }
            }
            let evidence=references.into_iter().map(|(path,lines,revision)| {
                match self.source(path,lines,revision){
                    Ok(text)=>json!({"path":path,"lines":lines,"revision":revision,"state":"readable","sha256":format!("{:x}",Sha256::digest(text.as_bytes()))}),
                    Err(error)=>json!({"path":path,"lines":lines,"revision":revision,"state":"unreadable","error":error.to_string()}),
                }
            }).collect();
            facts.sources.insert(node.id.clone(), evidence);
        }
        // Architecture and tier adapters are optional native inputs. Their
        // absence remains explicit null, never invented evidence or a tier.
        report::build(view, &facts).map_err(|e| e.to_string())
    }
    pub fn read(&self, view: &View, participant: &str) -> Result<Value, String> {
        let mut report = self.inspect(view)?;
        let digest = Review.digest(view).map_err(|e| e.to_string())?;
        let mut cursors = self
            .cursors
            .lock()
            .map_err(|_| "review cursor lock poisoned")?;
        let changes = match cursors.get(participant) {
            Some(before) => Review
                .changes(before, &digest, participant)
                .map_err(|e| e.to_string())?,
            None => vec![],
        };
        if !changes.is_empty() {
            let readback = report["readback"].as_str().unwrap_or_default();
            report["readback"] = json!(format!(
                "{readback}\nCHANGED SINCE YOUR LAST READ\n{}",
                changes.join("\n")
            ));
        }
        report["changes"] = json!(changes);
        cursors.insert(participant.into(), digest);
        Ok(report)
    }
}
impl Verifier for ReviewHost {
    fn verify(&self, change: &Change<'_>) -> Result<Value, CapError> {
        let mut facts = serde_json::Map::new();
        for (id, keys) in change.attempted {
            let Some(r) = change.after.get(id) else {
                continue;
            };
            if r.kind == "claim"
                && string(r, "basis") == "verified"
                && keys.iter().any(|k| {
                    ["about", "text", "basis", "path", "lines", "revision"].contains(&k.as_str())
                })
            {
                self.source(string(r, "path"), string(r, "lines"), string(r, "revision"))?;
                facts.insert(id.clone(),json!({"path":string(r,"path"),"lines":string(r,"lines"),"revision":string(r,"revision")}));
            }
        }
        Ok(Value::Object(facts))
    }
}

pub(crate) fn project_facts(root_path: &Path) -> Value {
    let run = |args: &[&str]| -> Result<String, String> {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root_path)
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        String::from_utf8(output.stdout).map_err(|e| e.to_string())
    };
    let root = root_path.display().to_string();
    let measured_root = run(&["rev-parse", "--show-toplevel"]);
    if measured_root
        .as_ref()
        .map(|r| r.trim() != root)
        .unwrap_or(true)
    {
        return json!({"state":"unavailable","reason":"The project root is not a Git repository root.","repository":root});
    }
    match (
        run(&["ls-files", "-z", "--cached"]),
        run(&["rev-parse", "--abbrev-ref", "HEAD"]),
        run(&["rev-parse", "--short", "HEAD"]),
    ) {
        (Ok(files), Ok(branch), Ok(revision)) => {
            let files: std::collections::BTreeSet<_> =
                files.split('\0').filter(|s| !s.is_empty()).collect();
            let mut languages = BTreeMap::<&str, usize>::new();
            for file in &files {
                let language = match file.rsplit('.').next().unwrap_or("") {
                    "rs" => "Rust",
                    "js" | "mjs" | "cjs" => "JavaScript",
                    "ts" | "tsx" => "TypeScript",
                    "py" => "Python",
                    "html" => "HTML",
                    "css" => "CSS",
                    _ => continue,
                };
                *languages.entry(language).or_default() += 1;
            }
            let mut languages: Vec<_> = languages.into_iter().collect();
            languages.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            json!({"state":"measured","repository":root,"branch":branch.trim(),"revision":revision.trim(),
                "tracked_files":files.len(),"cargo_manifests":files.iter().filter(|p| p.rsplit('/').next() == Some("Cargo.toml")).count(),
                "languages":languages,"commands":["git ls-files -z --cached","git rev-parse --abbrev-ref HEAD","git rev-parse --short HEAD"],
                "boundary":"Counts cover unique tracked paths, including tracked deletions; untracked files are excluded. Languages count file extensions. Cargo manifests are not a resolved package count."})
        }
        _ => {
            json!({"state":"unavailable","reason":"Could not read Git repository facts.","repository":root})
        }
    }
}

/// An omitted optional artifact leaves opaque records in the document. Map
/// remains usable, but deleting a referenced target needs the absent owner.
pub struct ReviewUnavailable;
impl Verifier for ReviewUnavailable {
    fn verify(&self, change: &Change<'_>) -> Result<Value, CapError> {
        if !change.delta.deleted.is_empty() {
            for record in change.before.records() {
                let target = match record.kind.as_str() {
                    "claim" => string(record, "about"),
                    "mark" => string(record, "target"),
                    _ => continue,
                };
                if change.after.contains(&record.id) && !change.after.contains(target) {
                    return Err(CapError::new("review_unavailable",format!("review capability is unavailable; deleting {target} requires its review cascade")));
                }
            }
        }
        Ok(Value::Null)
    }
}
