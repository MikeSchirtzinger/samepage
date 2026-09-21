import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock


REPO_ROOT = Path(__file__).resolve().parents[3]
RUNNER_PATH = REPO_ROOT / "dev/atlas-experiment-lib/runner.py"
SPEC = importlib.util.spec_from_file_location("atlas_experiment_runner", RUNNER_PATH)
assert SPEC is not None and SPEC.loader is not None
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class RunnerTests(unittest.TestCase):
    def test_default_manifest_is_valid(self):
        path = REPO_ROOT / RUNNER.DEFAULT_MANIFEST
        manifest = json.loads(path.read_text(encoding="utf-8"))
        RUNNER.validate_manifest(REPO_ROOT, manifest)

    def test_driver_path_cannot_escape_repository(self):
        path = REPO_ROOT / RUNNER.DEFAULT_MANIFEST
        manifest = json.loads(path.read_text(encoding="utf-8"))
        manifest["phases"][0]["driver"] = "../outside.mjs"
        with self.assertRaisesRegex(RUNNER.ExperimentError, "must stay inside"):
            RUNNER.validate_manifest(REPO_ROOT, manifest)

    def test_baseline_classifies_new_and_inherited_failures(self):
        baseline = {
            "run_id": "before",
            "gates": [
                {"id": "existing", "status": "fail"},
                {"id": "regressed", "status": "pass"},
            ],
            "phases": [{"id": "fixed", "status": "fail"}],
        }
        current = {
            "gates": [
                {"id": "existing", "status": "fail"},
                {"id": "regressed", "status": "fail"},
            ],
            "phases": [{"id": "fixed", "status": "pass"}],
        }
        comparison = RUNNER.compare_baseline(current, baseline)
        self.assertEqual(comparison["classifications"]["gate:existing"], "inherited-failure")
        self.assertEqual(comparison["classifications"]["gate:regressed"], "new-failure")
        self.assertEqual(comparison["classifications"]["phase:fixed"], "fixed")

    def test_json_write_is_atomic_and_round_trips(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "receipt.json"
            RUNNER.write_json(path, {"status": "pass"})
            self.assertEqual(RUNNER.read_json(path), {"status": "pass"})

    def test_pinned_tab_creation_times_out_fail_closed(self):
        timeout = RUNNER.subprocess.TimeoutExpired(["browser-tab"], 10)
        with mock.patch.dict(RUNNER.os.environ, {}, clear=True), mock.patch.object(
            RUNNER.subprocess, "run", side_effect=timeout
        ):
            with self.assertRaisesRegex(RUNNER.ExperimentError, "timed out after 10000 ms"):
                RUNNER.create_pinned_tab(REPO_ROOT, 9222)

    def test_existing_tab_pin_is_verified_and_not_owned(self):
        target_id = "ABC123"
        response = io.StringIO(
            json.dumps([{"id": target_id, "type": "page", "url": "about:blank"}])
        )
        with mock.patch.dict(RUNNER.os.environ, {"BROWSER_TAB_ID": target_id}), mock.patch.object(
            RUNNER.urllib.request, "urlopen", return_value=response
        ):
            tab_id, receipt, owned = RUNNER.create_pinned_tab(REPO_ROOT, 9222)
        self.assertEqual(tab_id, target_id)
        self.assertEqual(receipt["status"], "reused-existing-pin")
        self.assertFalse(owned)


if __name__ == "__main__":
    unittest.main()
