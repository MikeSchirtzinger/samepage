//! One read-only architecture review for the browser and attached agents.
use crate::source::Repo;
use same_page_atlas_core::Atlas;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub fn inspect(repo: &Repo, atlas: &Atlas, revision: &str) -> Value {
    let mut sources = BTreeMap::new();
    let mut nodes = Vec::new();
    for node in &atlas.nodes {
        let claims: Vec<_> = atlas
            .claims_of(&node.id)
            .into_iter()
            .filter(|c| c.live())
            .collect();
        let mut references: Vec<_> = claims
            .iter()
            .filter(|c| !c.path.is_empty())
            .map(|c| (c.path.as_str(), c.lines.as_str()))
            .collect();
        if !node.path.is_empty() {
            references.push((&node.path, &node.lines));
        }
        references.sort_unstable();
        references.dedup();
        let evidence: Vec<_> = references.into_iter().map(|(path, lines)| {
            sources.entry((path.to_string(), lines.to_string())).or_insert_with(|| {
                match repo.read(path, lines) {
                    Ok(excerpt) => json!({"path":path,"lines":lines,"state":if excerpt.truncated {"truncated"} else {"readable"},"sha256":format!("{:x}",Sha256::digest(excerpt.text.as_bytes()))}),
                    Err(error) => json!({"path":path,"lines":lines,"state":"unreadable","error":error}),
                }
            }).clone()
        }).collect();
        let incoming: Vec<_> = atlas
            .edges
            .iter()
            .filter(|e| e.to == node.id)
            .map(|e| &e.from)
            .collect();
        let outgoing: Vec<_> = atlas
            .edges
            .iter()
            .filter(|e| e.from == node.id)
            .map(|e| &e.to)
            .collect();
        let drafts: Vec<_> = atlas
            .decision_drafts
            .iter()
            .filter(|d| d.input.ids.contains(&node.id))
            .map(|d| &d.id)
            .collect();
        nodes.push(json!({"id":node.id,"label":node.label,"parent":node.parent,"evidence":evidence,
            "claims":claims,"challenge":atlas.challenge(&node.id),"incoming":incoming,"outgoing":outgoing,"draft_ids":drafts}));
    }
    json!({"schema_version":1,"document_revision":revision,
        "repository":repo.root().display().to_string(),
        "evidence_boundary":"Source checks read the working tree now. Readable source does not verify a diagram's prose or arrows. Commit-pinned claims describe their cited revision; freshness against current code has not been established. Connections are the authored diagram, not detected runtime calls.",
        "nodes":nodes,"decision_drafts":atlas.decision_drafts,
        "architecture":same_page_atlas_core::architecture::inspect(atlas).unwrap_or_else(|error| json!({"error":error})),
        "project_facts":project_facts(repo)})
}

/// [`inspect`] plus one computed trust tier per node (see `src/tier.rs`).
/// Kept as a wrapper rather than folded into `inspect` so the base function's
/// existing three-argument contract, and the test that calls it directly,
/// stay exactly as they were before tiers existed.
pub fn inspect_with_tiers(
    repo: &Repo,
    atlas: &Atlas,
    revision: &str,
    registry: &mut crate::tier::TierRegistry,
    registry_path: &std::path::Path,
) -> Value {
    let mut report = inspect(repo, atlas, revision);
    let g8 = crate::tier::read_g8_status(repo.root());
    let tiers = crate::tier::annotate(atlas, repo, registry, registry_path, &g8);
    if let Some(nodes) = report["nodes"].as_array_mut() {
        for node in nodes {
            let Some(id) = node["id"].as_str().map(str::to_string) else {
                continue;
            };
            if let Some(tier) = tiers.get(&id) {
                node["tier"] = tier.clone();
            }
        }
    }
    report["mode"] = json!(registry.mode);
    report
}

fn project_facts(repo: &Repo) -> Value {
    let run = |args: &[&str]| -> Result<String, String> {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repo.root())
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        String::from_utf8(output.stdout).map_err(|e| e.to_string())
    };
    let root = repo.root().display().to_string();
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

#[cfg(test)]
mod tests {
    use super::*;
    use ag_ui_canvas::scene::{Author, Scene};
    use same_page_atlas_core::{place_node, read, NodePatch};

    #[test]
    fn source_deletion_is_visible_and_readability_is_not_claim_verification() {
        let root = std::env::temp_dir().join(format!("atlas-review-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("lib.rs"), "pub struct Actual;\n").unwrap();
        let repo = Repo::open(&root).unwrap();
        let mut scene = Scene::new();
        place_node(
            &mut scene,
            &NodePatch {
                label: Some("Unproven description".into()),
                path: Some("lib.rs".into()),
                lines: Some("1".into()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .unwrap();
        let atlas = read(&scene).unwrap();
        let report = inspect(&repo, &atlas, "r1");
        assert_eq!(report["nodes"][0]["evidence"][0]["state"], "readable");
        assert_eq!(report["nodes"][0]["claims"], json!([]));
        std::fs::remove_file(root.join("lib.rs")).unwrap();
        assert_eq!(
            inspect(&repo, &atlas, "r1")["nodes"][0]["evidence"][0]["state"],
            "unreadable"
        );
        std::fs::remove_dir(root).unwrap();
    }
}
