//! Integration tests over the checked-in fixture tree and over this
//! repository itself. Run with `cargo test -p samepage-extract -- --nocapture`
//! to see the self-scan report printed as JSON.

use std::path::{Path, PathBuf};

use samepage_extract::{Lane, LaneKind, Report};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("the crate should sit two directories below the workspace root")
}

fn normalized(lane: &Lane) -> (LaneKind, String, u32) {
    (
        lane.kind,
        lane.evidence.path.to_string_lossy().replace('\\', "/"),
        lane.evidence.line,
    )
}

/// The complete, checked set of lanes the fixture tree must produce: no
/// more, no fewer. Any drift here means an extractor started matching (or
/// stopped matching) something it shouldn't.
#[test]
fn exact_lane_set_over_the_fixture_tree() {
    let report = samepage_extract::scan(&fixtures_root()).expect("fixture scan should succeed");

    let mut actual: Vec<(LaneKind, String, u32)> = report.lanes.iter().map(normalized).collect();
    actual.sort();

    let mut expected = vec![
        (LaneKind::Binary, "js-package/package.json".to_string(), 5),
        (LaneKind::Spawn, "js-package/bin/cli.js".to_string(), 4),
        (
            LaneKind::Binary,
            "rust-workspace/api/Cargo.toml".to_string(),
            2,
        ),
        (
            LaneKind::Listener,
            "rust-workspace/api/src/main.rs".to_string(),
            6,
        ),
        (
            LaneKind::Background,
            "rust-workspace/api/src/main.rs".to_string(),
            8,
        ),
        (
            LaneKind::Spawn,
            "rust-workspace/api/src/main.rs".to_string(),
            10,
        ),
        (
            LaneKind::Binary,
            "rust-workspace/worker/Cargo.toml".to_string(),
            2,
        ),
    ];
    expected.sort();

    assert_eq!(
        actual, expected,
        "lane set drifted from the fixture tree's checked contents\nfull report:\n{}",
        serde_json::to_string_pretty(&report).unwrap()
    );
}

/// Named for the product failure this crate exists to catch: a diagram
/// drawn from only one lane of the `api` service would already be wrong,
/// because the service both binds a socket AND spawns a sidecar process.
#[test]
fn a_sidecar_the_diagram_never_drew_is_a_lane() {
    let report = samepage_extract::scan(&fixtures_root()).expect("fixture scan should succeed");

    let api_main = Path::new("rust-workspace/api/src/main.rs");
    let has_listener = report
        .lanes
        .iter()
        .any(|l| l.kind == LaneKind::Listener && l.evidence.path == api_main);
    let has_spawn = report
        .lanes
        .iter()
        .any(|l| l.kind == LaneKind::Spawn && l.evidence.path == api_main);

    assert!(
        has_listener,
        "expected a Listener lane in {}: {:#?}",
        api_main.display(),
        report.lanes
    );
    assert!(
        has_spawn,
        "expected a Spawn lane in {}: {:#?}",
        api_main.display(),
        report.lanes
    );
}

/// A match written only in a comment, or living inside a `#[cfg(test)]`
/// module, must not become a lane — and a whole file under `tests/` must be
/// recorded as skipped rather than scanned at all.
#[test]
fn comment_and_test_module_matches_are_not_lanes() {
    let report = samepage_extract::scan(&fixtures_root()).expect("fixture scan should succeed");

    let worker_main = Path::new("rust-workspace/worker/src/main.rs");
    let worker_lanes: Vec<&Lane> = report
        .lanes
        .iter()
        .filter(|l| l.evidence.path == worker_main)
        .collect();
    assert_eq!(
        worker_lanes.len(),
        0,
        "worker/src/main.rs contains `TcpListener::bind` only in a comment and \
         inside a #[cfg(test)] module; it should yield zero lanes, got: {worker_lanes:#?}"
    );

    let integration_test = Path::new("rust-workspace/api/tests/integration.rs");
    let skipped_as_test_code = report
        .skipped
        .iter()
        .any(|s| s.path == integration_test && s.reason == "test code");
    assert!(
        skipped_as_test_code,
        "expected {} to be skipped as test code, got: {:#?}",
        integration_test.display(),
        report.skipped
    );
    assert!(
        !report
            .lanes
            .iter()
            .any(|l| l.evidence.path == integration_test),
        "a file under tests/ should never contribute a lane"
    );
}

/// The real check: run the scanner over this repository and confirm it
/// finds lanes this repo is actually known to have (a binary crate, a real
/// listener, and real process spawns in two different files), so a
/// single-lane picture of this codebase would be demonstrably wrong too.
#[test]
fn finds_known_lanes_in_this_repository() {
    let root = repo_root();
    let report: Report = samepage_extract::scan(&root).expect("self-scan should succeed");

    println!("{}", serde_json::to_string_pretty(&report).unwrap());

    let same_page_room_binary = report.lanes.iter().any(|l| {
        l.kind == LaneKind::Binary
            && l.label == "same-page-room"
            && l.evidence.path == Path::new("examples/same-page-room/Cargo.toml")
    });
    assert!(
        same_page_room_binary,
        "expected a Binary lane for the same-page-room crate"
    );

    let surface_listener = report.lanes.iter().any(|l| {
        l.kind == LaneKind::Listener
            && l.evidence.path == Path::new("crates/ag-ui-surface/src/lib.rs")
    });
    assert!(
        surface_listener,
        "expected a Listener lane in crates/ag-ui-surface/src/lib.rs (TcpListener::bind)"
    );

    let auth_spawn = report.lanes.iter().any(|l| {
        l.kind == LaneKind::Spawn && l.evidence.path == Path::new("crates/ag-ui-surface/src/auth.rs")
    });
    assert!(
        auth_spawn,
        "expected a Spawn lane in crates/ag-ui-surface/src/auth.rs (Command::new/.spawn())"
    );

    let turn_loop_spawn = report.lanes.iter().any(|l| {
        l.kind == LaneKind::Spawn
            && l.evidence.path == Path::new("crates/ag-ui-surface/src/turn_loop/pi.rs")
    });
    assert!(
        turn_loop_spawn,
        "expected a Spawn lane in crates/ag-ui-surface/src/turn_loop/pi.rs (Command::new/.spawn())"
    );
}

#[test]
fn an_import_of_command_is_not_a_spawn() {
    let dir = std::env::temp_dir().join(format!("samepage-extract-import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"imp\"\nversion = \"0.1.0\"\n").unwrap();
    std::fs::write(
        dir.join("src/main.rs"),
        "use std::process::Command;\nfn main() {\n    let _c = Command::new(\"node\").spawn();\n}\n",
    )
    .unwrap();
    let report = samepage_extract::scan(&dir).unwrap();
    let spawns: Vec<u32> = report
        .lanes
        .iter()
        .filter(|lane| lane.kind == samepage_extract::LaneKind::Spawn)
        .map(|lane| lane.evidence.line)
        .collect();
    assert_eq!(spawns, vec![3], "only the call is a lane, never the import: {report:?}");
}

#[test]
fn a_chained_spawn_is_the_same_sidecar_as_its_construction() {
    let dir = std::env::temp_dir().join(format!("samepage-extract-chain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"chain\"\nversion = \"0.1.0\"\n").unwrap();
    std::fs::write(
        dir.join("src/main.rs"),
        "fn main() {\n    let _side = std::process::Command::new(\"node\")\n        .arg(\"sidecar.js\")\n        .spawn()\n        .unwrap();\n    let other = std::process::Command::new(\"sh\");\n    let mut other = other;\n    let _o = other.spawn();\n}\n",
    )
    .unwrap();
    let report = samepage_extract::scan(&dir).unwrap();
    let spawns: Vec<u32> = report
        .lanes
        .iter()
        .filter(|lane| lane.kind == samepage_extract::LaneKind::Spawn)
        .map(|lane| lane.evidence.line)
        .collect();
    assert_eq!(spawns, vec![2, 6, 8], "one lane per construction, plus a bare spawn on a command built earlier: {report:?}");
}
