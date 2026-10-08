//! Extraction execution and sidecar persistence; never linked into Lanes WASM.
use super::sources::Repository;
use samepage_capability::{Call, CapError, Change, View};
use samepage_doc::host::Verifier;
use samepage_lanes::{Evidence, Facts, Lane, Registry, Removal, Scan};
use samepage_sources::{string, Facts as SourceFacts};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
};
pub fn lane_id(lane: &samepage_extract::Lane) -> String {
    let mut h = Sha256::new();
    h.update(lane.evidence.path.to_string_lossy().as_bytes());
    h.update(b":");
    h.update(lane.evidence.line.to_string().as_bytes());
    h.update(b":");
    h.update(format!("{:?}", lane.kind).as_bytes());
    format!("{:x}", h.finalize())[..16].into()
}
pub struct LanesHost {
    pub repo: Repository,
    pub registry: Mutex<Registry>,
    pub path: PathBuf,
}
impl LanesHost {
    pub fn open(root: &Path, state: &Path) -> Result<Self, String> {
        let path = state.with_file_name("lanes.json");
        let registry = match fs::read(&path) {
            Ok(b) => {
                serde_json::from_slice(&b).map_err(|e| format!("lanes sidecar is invalid: {e}"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Registry::default(),
            Err(e) => return Err(e.to_string()),
        };
        Ok(Self {
            repo: Repository::open(root)?,
            registry: Mutex::new(registry),
            path,
        })
    }
    fn scan(&self) -> Result<Scan, String> {
        let report = samepage_extract::scan(&self.repo.root).map_err(|e| e.to_string())?;
        let lanes = report
            .lanes
            .iter()
            .filter(|l| {
                self.repo
                    .check(
                        &l.evidence.path.to_string_lossy(),
                        &l.evidence.line.to_string(),
                    )
                    .is_ok()
            })
            .map(|l| Lane {
                lane_id: lane_id(l),
                kind: format!("{:?}", l.kind).to_lowercase(),
                label: l.label.clone(),
                package: l.package.clone(),
                detail: l.detail.clone(),
                evidence: Evidence {
                    path: l.evidence.path.to_string_lossy().into(),
                    line: l.evidence.line,
                    snippet: l.evidence.snippet.clone(),
                    sha256: l.evidence.sha256.clone(),
                },
            })
            .collect();
        Ok(Scan {
            root: self.repo.root.to_string_lossy().into(),
            lanes,
            scanned_files: report.scanned_files,
            languages: report.languages,
            skipped: report
                .skipped
                .into_iter()
                .map(|s| json!({"path":s.path,"reason":s.reason}))
                .collect(),
        })
    }
    pub fn save(&self, registry: &Registry) -> Result<(), String> {
        let tmp = self.path.with_extension("tmp");
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(registry).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        fs::rename(tmp, &self.path).map_err(|e| e.to_string())
    }
    pub fn facts(&self, view: &View, sources: SourceFacts) -> Result<Facts, String> {
        let scan = self.scan()?;
        let mut locked = self
            .registry
            .lock()
            .map_err(|_| "lanes registry poisoned")?;
        let mut next = locked.clone();
        for (id, l) in &next.last_seen {
            if !scan.lanes.iter().any(|n| &n.lane_id == id) {
                next.removals.entry(id.clone()).or_insert_with(|| Removal {
                    obligation_id: String::new(),
                    kind: l.kind.clone(),
                    label: l.label.clone(),
                    path: l.evidence.path.clone(),
                    line: l.evidence.line,
                    snippet: l.evidence.snippet.clone(),
                });
            }
        }
        for l in &scan.lanes {
            next.last_seen.insert(l.lane_id.clone(), l.clone());
            if next
                .removals
                .get(&l.lane_id)
                .is_some_and(|r| r.obligation_id.is_empty())
            {
                next.removals.remove(&l.lane_id);
            }
        }
        next.lane_claims = view
            .records()
            .filter(|r| r.kind == samepage_lanes::KIND)
            .map(|r| (string(r, "lane_id").into(), string(r, "node").into()))
            .collect();
        self.save(&next)?;
        *locked = next.clone();
        let mut facts = Facts {
            scan,
            registry: next,
            sources,
            ..Default::default()
        };
        for l in &facts.scan.lanes {
            let path = &l.evidence.path;
            let excerpt = self
                .repo
                .read(path, &l.evidence.line.to_string())
                .map_err(|e| e.to_string())?;
            facts.excerpt_hashes.insert(
                l.lane_id.clone(),
                format!("{:x}", Sha256::digest(excerpt.as_bytes())),
            );
            let mut bytes = Vec::new();
            fs::File::open(self.repo.root.join(path))
                .map_err(|e| e.to_string())?
                .take(2 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > 2 * 1024 * 1024 {
                return Err("lane file exceeds 2 MiB".into());
            }
            facts
                .file_hashes
                .insert(l.lane_id.clone(), format!("{:x}", Sha256::digest(bytes)));
        }
        Ok(facts)
    }
    pub fn inspect(&self, view: &View, sources: SourceFacts) -> Result<Value, String> {
        let facts = self.facts(view, sources)?;
        Ok(
            json!({"projection":samepage_lanes::reconcile(view,&facts),"facts":facts,"revision":view.revision()}),
        )
    }
    pub fn prepare(
        &self,
        args: Value,
        view: &View,
        sources: SourceFacts,
    ) -> Result<samepage_capability::Batch, String> {
        let facts = self.facts(view, sources)?;
        let plan = samepage_lanes::claim_plan(
            &Call {
                action: "lane_claim".into(),
                args,
            },
            view,
            &facts,
        )
        .map_err(|e| e.to_string())?;
        Ok(samepage_capability::Batch {
            base_revision: view.revision().into(),
            entries: vec![samepage_capability::BatchEntry {
                capability: "lanes".into(),
                plan,
            }],
        })
    }
}
impl Verifier for LanesHost {
    fn verify(&self, c: &Change<'_>) -> Result<Value, CapError> {
        let facts = self
            .facts(c.before, SourceFacts::default())
            .map_err(|e| CapError::new("lane_invalid", e))?;
        let mut out = serde_json::Map::new();
        for id in c.attempted.keys() {
            if let Some(r) = c.after.get(id).filter(|r| r.kind == samepage_lanes::KIND) {
                let lane = facts
                    .scan
                    .lanes
                    .iter()
                    .find(|l| l.lane_id == string(r, "lane_id"))
                    .ok_or_else(|| CapError::new("lane_invalid", "lane is no longer present"))?;
                let baseline = samepage_lanes::pin_check(lane, &facts)?;
                if string(r, "path") != lane.evidence.path
                    || string(r, "lines") != lane.evidence.line.to_string()
                    || string(r, "baseline_sha256") != baseline
                    || string(r, "scan_sha256") != lane.evidence.sha256
                {
                    return Err(CapError::new(
                        "lane_invalid",
                        "binding differs from current verified extraction evidence",
                    ));
                }
                out.insert(
                    id.clone(),
                    serde_json::to_value(&r.props)
                        .map_err(|e| CapError::new("lane_invalid", e.to_string()))?,
                );
            }
        }
        Ok(Value::Object(out))
    }
}
