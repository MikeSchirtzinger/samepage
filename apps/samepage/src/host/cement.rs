//! Native reviewed publication and real G8 execution. The document stamp is last.
use super::{
    lanes::LanesHost,
    sources::{Repository, SourcesHost},
};
use samepage_capability::{Batch, CapError, Change, Op, Value as ScalarValue, View};
use samepage_cement::{Bundle, BundleFacts, Coverage};
use samepage_doc::host::Verifier;
use samepage_sources::{GateFact, string};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};
const SPEC: &str = "specs/obligations-v0.1.json";
const INDEX: &str = "docs/decisions/index.json";
#[derive(Clone, Default, Serialize, Deserialize)]
struct Registry {
    gates: BTreeMap<String, (String, String)>,
}
#[derive(Clone)]
struct Publication {
    bundle: Bundle,
    pins: BTreeMap<String, Option<String>>,
    stamps: BTreeMap<String, String>,
}
pub struct CementHost {
    pub repo: Repository,
    registry: Mutex<Registry>,
    path: PathBuf,
    previews: Mutex<BTreeMap<String, Bundle>>,
    published: Mutex<BTreeMap<String, Publication>>,
}
impl CementHost {
    pub fn open(root: &Path, state: &Path) -> Result<Self, String> {
        let path = state.with_file_name("cement.json");
        let registry = match fs::read(&path) {
            Ok(b) => {
                serde_json::from_slice(&b).map_err(|e| format!("invalid Cement sidecar: {e}"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Registry::default(),
            Err(e) => return Err(e.to_string()),
        };
        Ok(Self {
            repo: Repository::open(root)?,
            registry: Mutex::new(registry),
            path,
            previews: Mutex::default(),
            published: Mutex::default(),
        })
    }
    fn safe(&self, name: &str) -> Result<PathBuf, String> {
        let p = Path::new(name);
        if p.is_absolute() || p.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err("publication path must be repository-relative".into());
        }
        let path = self.repo.root.join(p);
        let mut ancestor = path.as_path();
        while !ancestor.exists() {
            ancestor = ancestor.parent().ok_or("publication parent missing")?;
        }
        if !ancestor
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(&self.repo.root)
        {
            return Err("publication path leaves the repository".into());
        }
        Ok(path)
    }
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        match fs::read(self.safe(name)?) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
    fn pin(&self, name: &str) -> Result<Option<String>, String> {
        Ok(self.read(name)?.map(|b| samepage_cement::hash(&b)))
    }
    fn write(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let p = self.safe(name)?;
        fs::create_dir_all(p.parent().ok_or("publication has no parent")?)
            .map_err(|e| e.to_string())?;
        let temp = p.with_extension("samepage-staging");
        if temp.exists() {
            return Err(format!("staging file already exists: {}", temp.display()));
        }
        fs::write(&temp, bytes).map_err(|e| e.to_string())?;
        if let Err(e) = fs::rename(&temp, &p) {
            let _ = fs::remove_file(temp);
            return Err(e.to_string());
        }
        Ok(())
    }
    fn json(&self, name: &str) -> Result<Value, String> {
        self.read(name)?
            .map(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
            .unwrap_or(Ok(Value::Null))
    }
    fn run(&self, args: &[&str], allow_check_failure: bool) -> Result<Value, String> {
        let o = Command::new("g8")
            .args(args)
            .current_dir(&self.repo.root)
            .output()
            .map_err(|e| format!("real g8 could not run: {e}"))?;
        let value: Value = serde_json::from_slice(&o.stdout).map_err(|e| {
            format!(
                "g8 returned invalid JSON: {e}: {}",
                String::from_utf8_lossy(&o.stderr)
            )
        })?;
        if value["g8_version"] != "0.1.5" {
            return Err("reviewed adapter requires g8 0.1.5".into());
        }
        if value["errors"].as_array().is_some_and(|a| !a.is_empty())
            || value["obligations"]
                .as_array()
                .is_some_and(|a| a.iter().any(|o| o["status"] == "error"))
        {
            return Err(format!("g8 execution error: {value}"));
        }
        if !o.status.success()
            && (!allow_check_failure
                || value["obligations"].as_array().is_none()
                || value["errors"].as_array().is_none_or(|a| !a.is_empty()))
        {
            return Err(format!(
                "g8 failed: {value}; {}",
                String::from_utf8_lossy(&o.stderr)
            ));
        }
        Ok(value)
    }
    /// Refuse a decision draft whose evidence files cannot be read now,
    /// naming the check, instead of failing later at review.
    pub fn check_evidence(&self, args: &Value) -> Result<(), String> {
        for check in args["draft"]["checks"].as_array().into_iter().flatten() {
            for file in check["evidence_files"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                self.repo.check(file, "1").map_err(|e| {
                    format!(
                        "check {}: evidence file {file} cannot be read: {}",
                        check["id"].as_str().unwrap_or("?"),
                        e.message
                    )
                })?;
            }
        }
        Ok(())
    }
    pub fn check(&self) -> Result<Value, String> {
        self.run(&["check", "--enforce", "--json"], true)
    }
    fn verify_claims(&self, v: &View, ids: &[String]) -> Result<(), String> {
        for id in ids {
            for c in samepage_review::claims_of(v, id).into_iter().filter(|c| {
                !samepage_review::flag(c, "withdrawn") && string(c, "basis") == "verified"
            }) {
                self.repo
                    .excerpt(string(c, "path"), string(c, "lines"), string(c, "revision"))
                    .map_err(|e| format!("verified claim {}: {e}", c.id))?;
            }
        }
        Ok(())
    }
    fn facts(
        &self,
        v: &View,
        ids: &[String],
        draft_id: &str,
        at: &str,
    ) -> Result<BundleFacts, String> {
        let git = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(&self.repo.root)
            .output()
            .map_err(|e| e.to_string())?;
        if !git.status.success()
            || String::from_utf8_lossy(&git.stdout).trim() != self.repo.root.to_string_lossy()
        {
            return Err("reviewed publication requires the selected Git repository root".into());
        }
        self.verify_claims(v, ids)?;
        let d = samepage_cement::reviewed(v, draft_id, ids).map_err(|e| e.to_string())?;
        let index = self.json(INDEX)?;
        let number = index["decisions"]
            .as_array()
            .map(|a| a.len() as u32 + 1)
            .unwrap_or(1);
        let mut evidence = BTreeMap::new();
        for c in &d.input.checks {
            for file in &c.evidence_files {
                self.repo.check(file, "1").map_err(|e| e.to_string())?;
                evidence.insert(file.clone(), self.pin(file)?);
            }
        }
        Ok(BundleFacts {
            repository: self.repo.root.to_string_lossy().into(),
            at: at.into(),
            number,
            spec_hash: self.pin(SPEC)?,
            index_hash: self.pin(INDEX)?,
            evidence,
        })
    }
    pub fn preview(&self, v: &View, args: &Value, human: bool) -> Result<Value, String> {
        let ids = ids(args)?;
        let draft_id = args["draft_id"].as_str().ok_or("draft_id missing")?;
        let at = now()?;
        let facts = self.facts(v, &ids, draft_id, &at)?;
        let b = samepage_cement::bundle(v, &ids, draft_id, &facts).map_err(|e| e.to_string())?;
        if human {
            let mut p = self
                .previews
                .lock()
                .map_err(|_| "Cement preview lock poisoned")?;
            if p.len() >= 16 {
                p.clear();
            }
            p.insert(b.review_token.clone(), b.clone());
        }
        Ok(json!({"bundle":b,"facts":facts,"human_review":human}))
    }
    pub fn publish(self: &Arc<Self>, v: &View, args: &Value) -> Result<Pending, String> {
        let token = args["review_token"]
            .as_str()
            .ok_or("review token missing")?;
        let b = self
            .previews
            .lock()
            .map_err(|_| "Cement preview lock poisoned")?
            .remove(token)
            .ok_or("human preview is missing or consumed; review again")?;
        let ids = ids(args)?;
        if b.revision != v.revision() || b.draft.id != args["draft_id"].as_str().unwrap_or_default()
        {
            return Err("review does not match the current revision, selection or draft".into());
        }
        let facts = self.facts(
            v,
            &ids,
            &b.draft.id,
            b.receipt["review"]["cemented_at"]
                .as_str()
                .ok_or("review time missing")?,
        )?;
        let current =
            samepage_cement::bundle(v, &ids, &b.draft.id, &facts).map_err(|e| e.to_string())?;
        if current.review_token != token {
            return Err("repository or reviewed content changed; preview again".into());
        }
        let mut paths = vec![
            SPEC.to_string(),
            INDEX.into(),
            "g8.lock".into(),
            "specs/attestations-v0.1.json".into(),
            ".g8/config.toml".into(),
            ".g8/store.db".into(),
            ".g8/store.db-wal".into(),
            ".g8/store.db-shm".into(),
            ".g8/store.db-journal".into(),
            b.adr_path.clone(),
            b.fragment_path.clone(),
            format!("{}.receipt.json", b.adr_path),
        ];
        paths.sort();
        paths.dedup();
        let backups = paths
            .iter()
            .map(|p| Ok((p.clone(), self.read(p)?)))
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let mut pending = Pending {
            host: self.clone(),
            backups,
            token: token.into(),
            batch: Batch {
                base_revision: v.revision().into(),
                entries: vec![],
            },
            result: Value::Null,
            accepted: false,
        };
        let result = (|| {
            if self.read(&b.adr_path)?.is_some() || self.read(&b.fragment_path)?.is_some() {
                return Err("reviewed ADR destination already exists".into());
            }
            let fragment = serde_json::to_vec_pretty(&b.fragment).map_err(|e| e.to_string())?;
            self.write(&b.adr_path, b.adr.as_bytes())?;
            self.write(&b.fragment_path, &fragment)?;
            let merged = samepage_cement::merge(
                self.json(SPEC)?,
                b.fragment["obligations"]
                    .as_array()
                    .ok_or("fragment missing obligations")?
                    .clone(),
            )
            .map_err(|e| e.to_string())?;
            self.write(
                SPEC,
                &serde_json::to_vec_pretty(&merged).map_err(|e| e.to_string())?,
            )?;
            if self.read(".g8/config.toml")?.is_none() {
                self.run(
                    &[
                        "init",
                        "--no-claude-import",
                        "--no-subagents",
                        "--output",
                        "json",
                    ],
                    false,
                )?;
            }
            self.run(&["ratify", "--output", "json"], false)?;
            for c in &b.draft.input.checks {
                let mut command = vec!["attest", c.id.as_str(), "--files"];
                let files: Vec<_> = c
                    .evidence_files
                    .iter()
                    .map(|file| format!("./{file}"))
                    .collect();
                command.extend(files.iter().map(String::as_str));
                command.extend(["--output", "json"]);
                self.run(&command, false)?;
            }
            let gate = self.check()?;
            let selected: BTreeMap<_, _> = gate["obligations"]
                .as_array()
                .ok_or("G8 returned no obligations")?
                .iter()
                .filter_map(|o| Some((o["id"].as_str()?.to_string(), o.clone())))
                .collect();
            let passing = b
                .draft
                .input
                .checks
                .iter()
                .all(|c| selected.get(&c.id).is_some_and(|o| o["status"] == "passed"));
            if b.draft
                .input
                .checks
                .iter()
                .any(|c| !selected.contains_key(&c.id))
            {
                return Err("G8 omitted a reviewed obligation".into());
            }
            let status = if passing {
                "active_passing"
            } else {
                "active_blocked"
            };
            let mut index = self.json(INDEX)?;
            if index.is_null() {
                index = json!({"decisions":[]});
            }
            index["decisions"].as_array_mut().ok_or("ADR index invalid")?.push(json!({"id":b.draft.id,"title":b.draft.input.title,"status":"accepted","path":b.adr_path}));
            self.write(
                INDEX,
                &serde_json::to_vec_pretty(&index).map_err(|e| e.to_string())?,
            )?;
            let mut receipt = b.receipt.clone();
            receipt["g8_status"] = json!(status);
            receipt["g8_check"] = gate.clone();
            self.write(
                &format!("{}.receipt.json", b.adr_path),
                &serde_json::to_vec_pretty(&receipt).map_err(|e| e.to_string())?,
            )?;
            // Recheck all reviewed source evidence after the CLI, before document publication.
            self.verify_claims(v, &ids)?;
            for (file, pin) in &facts.evidence {
                if &self.pin(file)? != pin {
                    return Err(format!("evidence changed during publication: {file}"));
                }
            }
            let batch = samepage_cement::stamp(v, &b, status).map_err(|e| e.to_string())?;
            let mut stamps = BTreeMap::new();
            for entry in &batch.entries {
                for op in &entry.plan.ops {
                    if let Op::Set {
                        target: samepage_capability::Target::Id(id),
                        props,
                    } = op
                        && let Some(ScalarValue::Scalar(samepage_capability::Scalar::Str(s))) =
                            props.get("cemented")
                    {
                        stamps.insert(id.clone(), s.clone());
                    }
                }
            }
            let pins = paths
                .iter()
                .map(|p| Ok((p.clone(), self.pin(p)?)))
                .collect::<Result<BTreeMap<_, _>, String>>()?;
            self.published
                .lock()
                .map_err(|_| "Cement publication lock poisoned")?
                .insert(
                    token.into(),
                    Publication {
                        bundle: b.clone(),
                        pins,
                        stamps,
                    },
                );
            pending.batch = batch;
            pending.result = json!({"receipt":receipt,"bundle":b,"g8":gate});
            Ok(())
        })();
        if let Err(e) = result {
            let cleanup = pending.rollback();
            return Err(match cleanup {
                Ok(()) => e,
                Err(c) => format!("{e}; rollback failed: {c}"),
            });
        }
        Ok(pending)
    }
    pub fn gates(&self, v: &View) -> Result<(BTreeMap<String, GateFact>, Value), String> {
        let registry = self
            .registry
            .lock()
            .map_err(|_| "Cement registry poisoned")?
            .clone();
        let mut nodes: BTreeMap<String, (Vec<String>, bool)> = registry
            .gates
            .iter()
            .map(|(id, (obligation, content))| {
                (
                    id.clone(),
                    (
                        vec![obligation.clone()],
                        samepage_cement::content(v, std::slice::from_ref(id))
                            .is_ok_and(|h| h == *content),
                    ),
                )
            })
            .collect();
        for n in v.records().filter(|r| r.kind == "node") {
            if let Some(encoded) = string(n, "cemented").splitn(3, '\n').nth(2)
                && let Ok(c) = serde_json::from_str::<Coverage>(encoded)
            {
                nodes.insert(
                    n.id.clone(),
                    (c.obligation_ids, samepage_cement::covers(v, n)),
                );
            }
        }
        if nodes.is_empty() {
            return Ok((BTreeMap::new(), Value::Null));
        }
        let check = self.check()?;
        let obligations = check["obligations"]
            .as_array()
            .ok_or("G8 gate response missing obligations")?;
        let empty = Vec::new();
        let failures = check["enforcement_failures"].as_array().unwrap_or(&empty);
        let facts = nodes
            .into_iter()
            .map(|(id, (ids, current))| (id, gate_fact(&ids, current, obligations, failures)))
            .collect();
        Ok((facts, check))
    }
    pub fn map_cement(
        &self,
        v: &View,
        args: &Value,
        sources: &SourcesHost,
    ) -> Result<Value, String> {
        let ids = if args.get("ids").is_some() {
            ids(args)?
        } else {
            let agreed: Vec<_> = v
                .records()
                .filter(|r| r.kind == "node" && string(r, "status") == "agreed")
                .map(|r| format!("{:?} [{}]", string(r, "label"), r.id))
                .collect();
            let others = v.records().filter(|r| r.kind == "node").count() - agreed.len();
            return Err(if agreed.is_empty() {
                format!(
                    "Pass ids to cement the agreed subset. No card is agreed yet; a person agrees to a card first ({others} cards are open)."
                )
            } else {
                format!(
                    "Pass ids to cement the agreed subset. Agreed now: {}. {others} other cards are not agreed.",
                    agreed.join(", ")
                )
            });
        };
        samepage_cement::settled(v, &ids).map_err(|e| e.to_string())?;
        self.verify_claims(v, &ids)?;
        let sourcefacts = sources.facts(v)?;
        let mut excerpts = BTreeMap::new();
        for id in &ids {
            let n = v.get(id).ok_or("card missing")?;
            for b in samepage_sources::effective_bindings(
                n,
                &sourcefacts.extras.get(id).cloned().unwrap_or_default(),
            ) {
                excerpts.insert(
                    samepage_sources::binding_key(id, &b),
                    self.repo
                        .read(&b.path, &b.lines)
                        .map_err(|e| e.to_string())?,
                );
            }
        }
        let entries = samepage_cement::map_obligations(v, &ids, &sourcefacts, &excerpts)
            .map_err(|e| e.to_string())?;
        let merged =
            samepage_cement::merge(self.json(SPEC)?, entries.clone()).map_err(|e| e.to_string())?;
        let old = self.read(SPEC)?;
        self.write(
            SPEC,
            &serde_json::to_vec_pretty(&merged).map_err(|e| e.to_string())?,
        )?;
        let mut locked = self
            .registry
            .lock()
            .map_err(|_| "Cement registry poisoned")?;
        let mut next = locked.clone();
        for o in &entries {
            let id = o["node_id"].as_str().ok_or("node id missing")?;
            next.gates.insert(
                id.into(),
                (
                    o["id"].as_str().ok_or("obligation id missing")?.into(),
                    samepage_cement::content(v, &[id.into()]).map_err(|e| e.to_string())?,
                ),
            );
        }
        if let Err(e) = self.save_registry(&next) {
            self.restore(SPEC, old.as_deref())?;
            return Err(e);
        }
        *locked = next;
        Ok(
            json!({"path":SPEC,"obligations":entries,"state":"unratified and unattested; not proof of a met gate"}),
        )
    }
    fn save_registry(&self, r: &Registry) -> Result<(), String> {
        let temp = self.path.with_extension("tmp");
        fs::write(
            &temp,
            serde_json::to_vec_pretty(r).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        fs::rename(temp, &self.path).map_err(|e| e.to_string())
    }
    pub fn remove(
        &self,
        v: &View,
        args: &Value,
        lanes: &LanesHost,
        sources: &SourcesHost,
    ) -> Result<Value, String> {
        let f = lanes.facts(v, sources.facts(v)?)?;
        let id = args["lane_id"].as_str().ok_or("lane_id missing")?;
        let l = f
            .scan
            .lanes
            .iter()
            .find(|l| l.lane_id == id)
            .ok_or("lane not present in current scan")?;
        let o = samepage_cement::removal(l, args["reason"].as_str().unwrap_or_default())
            .map_err(|e| e.to_string())?;
        let merged =
            samepage_cement::merge(self.json(SPEC)?, vec![o.clone()]).map_err(|e| e.to_string())?;
        let old = self.read(SPEC)?;
        self.write(
            SPEC,
            &serde_json::to_vec_pretty(&merged).map_err(|e| e.to_string())?,
        )?;
        let mut locked = lanes
            .registry
            .lock()
            .map_err(|_| "Lanes registry poisoned")?;
        let mut next = locked.clone();
        next.removals.insert(
            id.into(),
            samepage_lanes::Removal {
                obligation_id: o["id"].as_str().ok_or("removal obligation missing")?.into(),
                kind: l.kind.clone(),
                label: l.label.clone(),
                path: l.evidence.path.clone(),
                line: l.evidence.line,
                snippet: l.evidence.snippet.clone(),
            },
        );
        if let Err(e) = lanes.save(&next) {
            self.restore(SPEC, old.as_deref())?;
            return Err(e);
        }
        *locked = next;
        Ok(json!({"obligation":o,"state":"unratified and unattested"}))
    }
    pub fn history(&self) -> Result<Value, String> {
        let index = self.json(INDEX)?;
        let mut out = Vec::new();
        for d in index["decisions"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .take(256)
        {
            let p = d["path"].as_str().ok_or("ADR path missing")?;
            if !p.starts_with("docs/decisions/") {
                return Err("ADR outside decisions".into());
            }
            let body = self.read(p)?.ok_or("ADR missing")?;
            if body.len() > 131072 {
                return Err("ADR too large".into());
            }
            out.push(json!({"entry":d,"body":String::from_utf8(body).map_err(|e|e.to_string())?}));
        }
        Ok(json!(out))
    }
    fn restore(&self, path: &str, b: Option<&[u8]>) -> Result<(), String> {
        if let Some(b) = b {
            self.write(path, b)
        } else {
            let p = self.safe(path)?;
            if p.exists() {
                fs::remove_file(p).map_err(|e| e.to_string())?;
            }
            Ok(())
        }
    }
}
impl Verifier for CementHost {
    fn verify(&self, c: &Change<'_>) -> Result<Value, CapError> {
        let mut facts = serde_json::Map::new();
        for (id, keys) in c.attempted {
            if !keys.contains("cemented") {
                continue;
            }
            let r = c
                .after
                .get(id)
                .ok_or_else(|| CapError::new("publication_required", "node missing"))?;
            let published = self
                .published
                .lock()
                .map_err(|_| CapError::new("publication_required", "publication lock poisoned"))?;
            let p = published
                .values()
                .find(|p| p.stamps.get(id).is_some_and(|s| s == string(r, "cemented")))
                .ok_or_else(|| {
                    CapError::new("publication_required", "reviewed publication missing")
                })?;
            samepage_cement::reviewed(c.before, &p.bundle.draft.id, &p.bundle.draft.input.ids)?;
            if c.before.revision() != p.bundle.revision {
                return Err(CapError::new(
                    "publication_required",
                    "document changed during publication",
                ));
            }
            for (name, pin) in &p.pins {
                if self
                    .pin(name)
                    .map_err(|e| CapError::new("publication_required", e))?
                    != *pin
                {
                    return Err(CapError::new(
                        "publication_required",
                        format!("published file changed: {name}"),
                    ));
                }
            }
            facts.insert(id.clone(), json!(string(r, "cemented")));
        }
        Ok(Value::Object(facts))
    }
}
pub struct Pending {
    host: Arc<CementHost>,
    backups: BTreeMap<String, Option<Vec<u8>>>,
    token: String,
    pub batch: Batch,
    pub result: Value,
    accepted: bool,
}
impl Pending {
    pub fn accept(mut self) {
        self.accepted = true;
        let _ = self
            .host
            .published
            .lock()
            .map(|mut p| p.remove(&self.token));
    }
    pub fn rollback(&mut self) -> Result<(), String> {
        let mut errors = Vec::new();
        for (p, b) in &self.backups {
            if let Err(e) = self.host.restore(p, b.as_deref()) {
                errors.push(e);
            }
        }
        self.accepted = true;
        let _ = self
            .host
            .published
            .lock()
            .map(|mut p| p.remove(&self.token));
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        if !self.accepted
            && let Err(e) = self.rollback()
        {
            eprintln!("Cement rollback failed: {e}");
        }
    }
}
fn ids(a: &Value) -> Result<Vec<String>, String> {
    serde_json::from_value(a["ids"].clone()).map_err(|e| e.to_string())
}
fn now() -> Result<String, String> {
    let o = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err("native clock failed".into());
    }
    String::from_utf8(o.stdout)
        .map(|s| s.trim().into())
        .map_err(|e| e.to_string())
}

/// One card's gate from a real `g8 check --enforce` result.
///
/// Every obligation can pass its own check while the gate still fails: an
/// unratified spec or an obligation without fresh attestation is an
/// enforcement failure. Only the enforced gate decides "met".
fn gate_fact(ids: &[String], current: bool, obligations: &[Value], failures: &[Value]) -> GateFact {
    let rows: Vec<_> = ids
        .iter()
        .filter_map(|id| obligations.iter().find(|o| o["id"] == *id))
        .collect();
    let spec_wide: Vec<_> = failures
        .iter()
        .filter(|f| f["obligation_id"].as_str().is_none_or(str::is_empty))
        .collect();
    let blocked: Vec<_> = failures
        .iter()
        .filter(|f| {
            f["obligation_id"]
                .as_str()
                .is_some_and(|o| ids.iter().any(|id| id == o))
        })
        .collect();
    let summaries = |rows: &[&Value]| {
        rows.iter()
            .map(|r| {
                r["evidence"]["summary"]
                    .as_str()
                    .unwrap_or("obligation has no result")
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    let passed =
        !ids.is_empty() && rows.len() == ids.len() && rows.iter().all(|o| o["status"] == "passed");
    let met = current && passed && blocked.is_empty() && spec_wide.is_empty();
    let (blocker, reason) = if !current {
        (
            "changed",
            "Reviewed selection changed; old cement is history.".to_string(),
        )
    } else if rows.len() != ids.len() || ids.is_empty() {
        (
            "missing",
            format!(
                "Published obligation missing from real G8 result: {}",
                ids.join(", ")
            ),
        )
    } else if !passed {
        let failed: Vec<_> = rows
            .iter()
            .copied()
            .filter(|o| o["status"] != "passed")
            .collect();
        ("failed", summaries(&failed))
    } else if let Some(first) = spec_wide.first().or(blocked.first()) {
        (
            blocker_word(first["classification"].as_str().unwrap_or_default()),
            spec_wide
                .iter()
                .chain(&blocked)
                .map(|f| enforcement_reason(f))
                .collect::<Vec<_>>()
                .join("; "),
        )
    } else {
        ("", summaries(&rows))
    };
    GateFact {
        obligation_id: ids.join(","),
        met,
        reason,
        blocker: blocker.into(),
    }
}

/// The gate word a person sees for one `g8 check --enforce` failure.
fn blocker_word(classification: &str) -> &'static str {
    match classification {
        "unratified_spec_change" | "unratified" => "unratified",
        "insufficient_rigor" => "unattested",
        _ => "blocked",
    }
}

/// One enforcement failure in plain words, with the command a person runs.
fn enforcement_reason(failure: &Value) -> String {
    let classification = failure["classification"].as_str().unwrap_or_default();
    let detail = failure["detail"].as_str().unwrap_or_default();
    match (
        blocker_word(classification),
        failure["obligation_id"].as_str(),
    ) {
        ("unratified", _) => format!(
            "UNRATIFIED: {SPEC} changed after its last ratification; a person reviews it and runs `g8 ratify`"
        ),
        ("unattested", Some(id)) => format!(
            "UNATTESTED: {id} passes its check but has no fresh attestation; a person reviews the evidence and runs `g8 attest {id} --files <evidence>`"
        ),
        (_, Some(id)) => format!("{}: {id}: {detail}", classification.to_uppercase()),
        (_, None) => format!("{}: {detail}", classification.to_uppercase()),
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    fn passed(id: &str) -> Value {
        json!({"id":id,"status":"passed","evidence":{"summary":"rg match found"}})
    }

    #[test]
    fn a_passing_check_without_attestation_is_not_a_met_gate() {
        let ids = vec!["ATLAS-CARD-a".to_string()];
        let failures = [
            json!({"classification":"insufficient_rigor","obligation_id":"ATLAS-CARD-a",
            "detail":"gate requires verified or fresh attested evidence"}),
        ];
        let fact = gate_fact(&ids, true, &[passed("ATLAS-CARD-a")], &failures);
        assert!(!fact.met);
        assert_eq!(fact.blocker, "unattested");
        assert!(
            fact.reason.starts_with("UNATTESTED: ATLAS-CARD-a"),
            "{}",
            fact.reason
        );
        assert!(fact.reason.contains("g8 attest ATLAS-CARD-a"));
    }

    #[test]
    fn an_unratified_spec_blocks_every_gate_and_names_ratify() {
        let ids = vec!["ATLAS-CARD-a".to_string()];
        let failures = [
            json!({"classification":"unratified_spec_change","detail":"specs changed after ratification"}),
            json!({"classification":"insufficient_rigor","obligation_id":"ATLAS-CARD-a","detail":"x"}),
        ];
        let fact = gate_fact(&ids, true, &[passed("ATLAS-CARD-a")], &failures);
        assert!(!fact.met);
        assert_eq!(fact.blocker, "unratified");
        assert!(fact.reason.contains("UNRATIFIED") && fact.reason.contains("g8 ratify"));
        assert!(
            fact.reason.contains("UNATTESTED"),
            "both reasons are listed: {}",
            fact.reason
        );
    }

    #[test]
    fn a_failure_for_another_obligation_does_not_block_this_gate() {
        let ids = vec!["ATLAS-CARD-a".to_string()];
        let failures = [
            json!({"classification":"insufficient_rigor","obligation_id":"ATLAS-CARD-b","detail":"x"}),
        ];
        let fact = gate_fact(&ids, true, &[passed("ATLAS-CARD-a")], &failures);
        assert!(fact.met);
        assert_eq!(fact.blocker, "");
    }

    #[test]
    fn changed_missing_and_failed_obligations_are_unmet_with_their_reason() {
        let ids = vec!["ATLAS-CARD-a".to_string()];
        assert_eq!(
            gate_fact(&ids, false, &[passed("ATLAS-CARD-a")], &[]).blocker,
            "changed"
        );
        assert_eq!(gate_fact(&ids, true, &[], &[]).blocker, "missing");
        let failed =
            json!({"id":"ATLAS-CARD-a","status":"failed","evidence":{"summary":"no match"}});
        let fact = gate_fact(&ids, true, &[failed], &[]);
        assert_eq!(
            (fact.met, fact.blocker.as_str(), fact.reason.as_str()),
            (false, "failed", "no match")
        );
    }
}
