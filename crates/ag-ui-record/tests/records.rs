//! Fixture-driven validator tests.
//!
//! Every `tests/fixtures/*.json` file is a record. A file named
//! `neg-<check-slug>.json` must fail exactly the check whose slug it names; any
//! other file (including a real session fixture added later) must pass. This
//! means a real session record slots in by dropping one JSON file here, with no
//! code changes.

use std::path::{Path, PathBuf};

use ag_ui_record::{validate, Check, Record, RECORD_VERSION};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read fixtures dir {}: {error}", dir.display()))
        .map(|entry| entry.expect("fixture directory entry").path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    files.sort();
    files
}

#[test]
fn fixture_directory_runs_and_names_every_record() {
    let dir = fixtures_dir();
    let files = json_files(&dir);
    assert!(
        !files.is_empty(),
        "no fixture files under {}",
        dir.display()
    );

    let mut ran = Vec::new();
    let mut failures = Vec::new();
    let mut negative_slugs = Vec::new();

    for path in &files {
        let stem = path
            .file_stem()
            .expect("fixture file stem")
            .to_string_lossy()
            .to_string();
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));

        let record = match serde_json::from_str::<Record>(&text) {
            Ok(record) => record,
            Err(error) => {
                failures.push(format!("{stem}: failed to parse: {error}"));
                continue;
            }
        };
        if record.record_version != RECORD_VERSION {
            failures.push(format!(
                "{stem}: record_version {:?} is not supported (expected {RECORD_VERSION})",
                record.record_version
            ));
            continue;
        }

        let violations = validate(&record);
        let checks: Vec<&str> = violations.iter().map(|v| v.check.slug()).collect();

        if let Some(slug) = stem.strip_prefix("neg-") {
            negative_slugs.push(slug.to_string());
            match Check::from_slug(slug) {
                None => failures.push(format!("{stem}: unknown check slug {slug:?}")),
                Some(expected) => {
                    ran.push(format!("{stem}: FAIL ({})", checks.join(",")));
                    if checks != vec![expected.slug()] {
                        failures.push(format!(
                            "{stem}: expected to fail exactly {slug}, got {:?}",
                            checks
                        ));
                    }
                }
            }
        } else if violations.is_empty() {
            ran.push(format!("{stem}: PASS"));
        } else {
            failures.push(format!("{stem}: expected pass, got {:?}", checks));
        }
    }

    // One negative fixture per check, and no check left untested.
    let mut expected_slugs: Vec<&str> = Check::ALL.iter().map(|check| check.slug()).collect();
    expected_slugs.sort_unstable();
    let mut seen_slugs = negative_slugs.clone();
    seen_slugs.sort_unstable();
    if seen_slugs != expected_slugs {
        failures.push(format!(
            "negative fixture coverage mismatch: expected {:?}, got {:?}",
            expected_slugs, seen_slugs
        ));
    }

    eprintln!("fixtures ran ({}):", ran.len());
    for line in &ran {
        eprintln!("  {line}");
    }

    assert!(
        failures.is_empty(),
        "fixture failures:\n{}",
        failures.join("\n")
    );
    assert!(!ran.is_empty(), "no fixture was classified");
}
