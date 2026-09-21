#!/usr/bin/env python3
"""Fail-closed lifecycle runner for real Atlas browser experiments."""

from __future__ import annotations

import argparse
import contextlib
import datetime as dt
import json
import os
import re
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any


SCHEMA_VERSION = 1
DEFAULT_MANIFEST = "dev/atlas-experiment-lib/experiments/same-page-atlas-epsilon-lesson.json"
TAB_PATTERN = re.compile(r"(?:export\s+)?BROWSER_TAB_ID=([A-Fa-f0-9]+)")
TAB_CREATE_TIMEOUT_SECONDS = 10
PLACEHOLDERS = frozenset({"repo_root", "run_dir", "state_dir", "phase_dir"})


class ExperimentError(RuntimeError):
    """An expected, receipt-worthy experiment failure."""


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")


def elapsed_ms(started: float) -> float:
    return round((time.monotonic() - started) * 1000, 3)


def read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ExperimentError(f"cannot read JSON from {path}: {error}") from error
    if not isinstance(value, dict):
        raise ExperimentError(f"{path} must contain one JSON object")
    return value


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=path.parent
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            output.write(json.dumps(value, indent=2, sort_keys=True) + "\n")
            output.flush()
            os.fsync(output.fileno())
        temporary.replace(path)
    finally:
        with contextlib.suppress(FileNotFoundError):
            temporary.unlink()


def require_keys(value: dict[str, Any], keys: set[str], field: str) -> None:
    missing = sorted(keys.difference(value))
    if missing:
        raise ExperimentError(f"{field} is missing required keys: {', '.join(missing)}")


def require_string(value: Any, field: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ExperimentError(f"{field} must be a nonempty string")
    return value


def require_positive_int(value: Any, field: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
        raise ExperimentError(f"{field} must be a positive integer")
    return value


def require_command(value: Any, field: str) -> list[str]:
    if not isinstance(value, list) or not value:
        raise ExperimentError(f"{field} must be a nonempty command array")
    command = [require_string(part, f"{field}[]") for part in value]
    if any("\x00" in part for part in command):
        raise ExperimentError(f"{field} contains a null byte")
    return command


def resolve_repo_path(repo_root: Path, value: Any, field: str, *, must_exist: bool = True) -> Path:
    raw = require_string(value, field)
    candidate = (repo_root / raw).resolve()
    try:
        candidate.relative_to(repo_root)
    except ValueError as error:
        raise ExperimentError(f"{field} must stay inside the repository") from error
    if must_exist and not candidate.exists():
        raise ExperimentError(f"{field} does not exist: {raw}")
    return candidate


def validate_manifest(repo_root: Path, manifest: dict[str, Any]) -> None:
    require_keys(manifest, {"schema_version", "id", "description", "browser", "phases"}, "manifest")
    if manifest["schema_version"] != SCHEMA_VERSION:
        raise ExperimentError(
            f"unsupported manifest schema_version {manifest['schema_version']!r}; expected {SCHEMA_VERSION}"
        )
    experiment_id = require_string(manifest["id"], "manifest.id")
    if re.fullmatch(r"[a-z0-9][a-z0-9-]*", experiment_id) is None:
        raise ExperimentError("manifest.id must contain lowercase letters, digits, and hyphens")
    require_string(manifest["description"], "manifest.description")

    browser = manifest["browser"]
    if not isinstance(browser, dict):
        raise ExperimentError("manifest.browser must be an object")
    require_keys(browser, {"port", "start_command", "stop_command"}, "manifest.browser")
    require_positive_int(browser["port"], "manifest.browser.port")
    require_command(browser["start_command"], "manifest.browser.start_command")
    require_command(browser["stop_command"], "manifest.browser.stop_command")

    host = manifest.get("host")
    if host is not None:
        if not isinstance(host, dict):
            raise ExperimentError("manifest.host must be an object or null")
        require_keys(
            host,
            {"command", "cwd", "environment", "ready_url", "ready_timeout_ms", "shutdown_timeout_ms"},
            "manifest.host",
        )
        require_command(host["command"], "manifest.host.command")
        resolve_repo_path(repo_root, host["cwd"], "manifest.host.cwd")
        if not isinstance(host["environment"], dict):
            raise ExperimentError("manifest.host.environment must be an object")
        for key, value in host["environment"].items():
            require_string(key, "manifest.host.environment key")
            require_string(value, f"manifest.host.environment.{key}")
        require_string(host["ready_url"], "manifest.host.ready_url")
        require_positive_int(host["ready_timeout_ms"], "manifest.host.ready_timeout_ms")
        require_positive_int(host["shutdown_timeout_ms"], "manifest.host.shutdown_timeout_ms")

    gates = manifest.get("gates", [])
    if not isinstance(gates, list):
        raise ExperimentError("manifest.gates must be an array")
    gate_ids: set[str] = set()
    for index, gate in enumerate(gates):
        field = f"manifest.gates[{index}]"
        if not isinstance(gate, dict):
            raise ExperimentError(f"{field} must be an object")
        require_keys(gate, {"id", "phase", "required", "command"}, field)
        gate_id = require_string(gate["id"], f"{field}.id")
        if gate_id in gate_ids:
            raise ExperimentError(f"duplicate gate id: {gate_id}")
        gate_ids.add(gate_id)
        if gate["phase"] not in {"before", "after"}:
            raise ExperimentError(f"{field}.phase must be before or after")
        if not isinstance(gate["required"], bool):
            raise ExperimentError(f"{field}.required must be a boolean")
        require_command(gate["command"], f"{field}.command")

    phases = manifest["phases"]
    if not isinstance(phases, list) or not phases:
        raise ExperimentError("manifest.phases must be a nonempty array")
    phase_ids: set[str] = set()
    for index, phase in enumerate(phases):
        field = f"manifest.phases[{index}]"
        if not isinstance(phase, dict):
            raise ExperimentError(f"{field} must be an object")
        require_keys(phase, {"id", "url", "driver", "timeout_ms", "restart_host", "input"}, field)
        phase_id = require_string(phase["id"], f"{field}.id")
        if phase_id in phase_ids:
            raise ExperimentError(f"duplicate phase id: {phase_id}")
        phase_ids.add(phase_id)
        require_string(phase["url"], f"{field}.url")
        resolve_repo_path(repo_root, phase["driver"], f"{field}.driver")
        require_positive_int(phase["timeout_ms"], f"{field}.timeout_ms")
        if not isinstance(phase["restart_host"], bool):
            raise ExperimentError(f"{field}.restart_host must be a boolean")
        if phase["restart_host"] and host is None:
            raise ExperimentError(f"{field}.restart_host requires manifest.host")
        if not isinstance(phase["input"], dict):
            raise ExperimentError(f"{field}.input must be an object")


def render_placeholders(value: str, context: dict[str, Path]) -> str:
    fields = set(re.findall(r"\{([^{}]+)\}", value))
    unknown = sorted(fields.difference(PLACEHOLDERS))
    if unknown:
        raise ExperimentError(f"unknown placeholder(s): {', '.join(unknown)}")
    return value.format_map({key: str(path) for key, path in context.items()})


def port_is_open(port: int) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as connection:
        connection.settimeout(0.25)
        return connection.connect_ex(("127.0.0.1", port)) == 0


def wait_for_port(port: int, timeout_ms: int, label: str) -> None:
    deadline = time.monotonic() + (timeout_ms / 1000)
    while time.monotonic() < deadline:
        if port_is_open(port):
            return
        time.sleep(0.1)
    raise ExperimentError(f"{label} did not open port {port} within {timeout_ms} ms")


def wait_for_url(url: str, timeout_ms: int, process: subprocess.Popen[str]) -> None:
    deadline = time.monotonic() + (timeout_ms / 1000)
    last_error = "no response"
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise ExperimentError(f"host exited before readiness with code {process.returncode}")
        try:
            with urllib.request.urlopen(url, timeout=1) as response:
                if response.status < 500:
                    return
                last_error = f"HTTP {response.status}"
        except (urllib.error.URLError, TimeoutError, OSError) as error:
            last_error = str(error)
        time.sleep(0.1)
    raise ExperimentError(f"host readiness failed for {url}: {last_error}")


def run_logged_command(
    command: list[str], cwd: Path, log_path: Path, environment: dict[str, str] | None = None
) -> dict[str, Any]:
    started = time.monotonic()
    with log_path.open("w", encoding="utf-8") as log:
        completed = subprocess.run(
            command,
            cwd=cwd,
            env=environment,
            stdout=log,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
        )
    return {
        "command": command,
        "exit_code": completed.returncode,
        "status": "pass" if completed.returncode == 0 else "fail",
        "duration_ms": elapsed_ms(started),
        "log": str(log_path),
    }


def start_host(
    repo_root: Path,
    host: dict[str, Any],
    context: dict[str, Path],
    host_log: Path,
) -> tuple[subprocess.Popen[str], dict[str, Any]]:
    environment = os.environ.copy()
    for key, value in host["environment"].items():
        environment[key] = render_placeholders(value, context)
    command = [render_placeholders(part, context) for part in host["command"]]
    cwd = resolve_repo_path(repo_root, host["cwd"], "manifest.host.cwd")
    log_handle = host_log.open("a", encoding="utf-8")
    started = time.monotonic()
    try:
        process = subprocess.Popen(
            command,
            cwd=cwd,
            env=environment,
            stdout=log_handle,
            stderr=subprocess.STDOUT,
            text=True,
            start_new_session=True,
        )
    finally:
        log_handle.close()
    try:
        wait_for_url(host["ready_url"], host["ready_timeout_ms"], process)
    except Exception:
        stop_process(process, host["shutdown_timeout_ms"])
        raise
    return process, {
        "command": command,
        "cwd": str(cwd),
        "ready_url": host["ready_url"],
        "ready_ms": elapsed_ms(started),
        "log": str(host_log),
    }


def stop_process(process: subprocess.Popen[str] | None, timeout_ms: int) -> dict[str, Any] | None:
    if process is None:
        return None
    started = time.monotonic()
    forced = False
    if process.poll() is None:
        with contextlib.suppress(ProcessLookupError):
            os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=timeout_ms / 1000)
        except subprocess.TimeoutExpired:
            forced = True
            with contextlib.suppress(ProcessLookupError):
                os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=2)
    return {
        "exit_code": process.returncode,
        "forced": forced,
        "stop_ms": elapsed_ms(started),
    }


def start_browser(browser: dict[str, Any], repo_root: Path, log_path: Path) -> tuple[bool, dict[str, Any]]:
    port = browser["port"]
    if port_is_open(port):
        return False, {"owned": False, "port": port, "status": "reused-existing-browser"}
    result = run_logged_command(browser["start_command"], repo_root, log_path)
    if result["exit_code"] != 0:
        raise ExperimentError(f"browser start failed with code {result['exit_code']}; see {log_path}")
    try:
        wait_for_port(port, 20_000, "browser")
    except Exception:
        stop_browser(browser, repo_root, log_path.with_name("browser-stop-after-start-failure.log"))
        raise
    return True, {"owned": True, "port": port, "status": "started-by-runner", "start": result}


def stop_browser(browser: dict[str, Any], repo_root: Path, log_path: Path) -> dict[str, Any]:
    return run_logged_command(browser["stop_command"], repo_root, log_path)


def create_pinned_tab(
    repo_root: Path, browser_port: int
) -> tuple[str, dict[str, Any], bool]:
    existing_pin = os.environ.get("BROWSER_TAB_ID", "").strip()
    if existing_pin:
        if re.fullmatch(r"[A-Fa-f0-9]+", existing_pin) is None:
            raise ExperimentError("BROWSER_TAB_ID must be a hexadecimal Chrome target id")
        try:
            with urllib.request.urlopen(
                f"http://127.0.0.1:{browser_port}/json/list", timeout=2
            ) as response:
                targets = json.load(response)
        except (urllib.error.URLError, TimeoutError, OSError, json.JSONDecodeError) as error:
            raise ExperimentError(f"cannot verify BROWSER_TAB_ID: {error}") from error
        if not isinstance(targets, list) or not any(
            isinstance(target, dict)
            and target.get("type") == "page"
            and target.get("id") == existing_pin
            for target in targets
        ):
            raise ExperimentError(f"pinned Chrome tab {existing_pin} is unavailable")
        return existing_pin, {
            "status": "reused-existing-pin",
            "tab_id": existing_pin,
        }, False
    try:
        completed = subprocess.run(
            ["browser-tab"],
            cwd=repo_root,
            capture_output=True,
            text=True,
            check=False,
            timeout=TAB_CREATE_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired as error:
        raise ExperimentError(
            f"browser-tab timed out after {TAB_CREATE_TIMEOUT_SECONDS * 1000} ms"
        ) from error
    match = TAB_PATTERN.search(completed.stdout)
    if completed.returncode != 0 or match is None:
        detail = completed.stderr.strip() or completed.stdout.strip() or "no diagnostic"
        raise ExperimentError(f"browser-tab failed: {detail}")
    return match.group(1), {
        "status": "created-by-browser-tab",
        "stdout": completed.stdout.strip(),
        "stderr": completed.stderr.strip(),
    }, True


def close_tab(browser_port: int, tab_id: str) -> None:
    request = urllib.request.Request(
        f"http://127.0.0.1:{browser_port}/json/close/{tab_id}",
        method="GET",
    )
    with contextlib.suppress(urllib.error.URLError, OSError):
        urllib.request.urlopen(request, timeout=2).close()


def run_phase(
    repo_root: Path,
    browser: dict[str, Any],
    phase: dict[str, Any],
    phase_dir: Path,
) -> dict[str, Any]:
    phase_dir.mkdir(parents=True, exist_ok=False)
    tab_id, tab_receipt, owned_tab = create_pinned_tab(repo_root, browser["port"])
    input_path = phase_dir / "driver-input.json"
    write_json(input_path, phase["input"])
    result_path = phase_dir / "browser-result.json"
    driver_path = resolve_repo_path(repo_root, phase["driver"], f"phase {phase['id']} driver")
    command = [
        "node",
        str(repo_root / "dev/atlas-experiment-lib/direct-cdp.mjs"),
        "--port",
        str(browser["port"]),
        "--tab",
        tab_id,
        "--url",
        phase["url"],
        "--driver",
        str(driver_path),
        "--input",
        str(input_path),
        "--output",
        str(phase_dir),
        "--result",
        str(result_path),
        "--timeout-ms",
        str(phase["timeout_ms"]),
    ]
    log_path = phase_dir / "driver.log"
    try:
        command_receipt = run_logged_command(command, repo_root, log_path)
        result = read_json(result_path) if result_path.exists() else {
            "status": "fail",
            "error": "direct CDP driver did not write a result",
        }
        return {
            "id": phase["id"],
            "status": "pass" if command_receipt["exit_code"] == 0 and result.get("status") == "pass" else "fail",
            "tab_id": tab_id,
            "tab": tab_receipt,
            "command": command_receipt,
            "result": result,
        }
    finally:
        if owned_tab:
            close_tab(browser["port"], tab_id)


def result_map(receipt: dict[str, Any]) -> dict[str, str]:
    results: dict[str, str] = {}
    for gate in receipt.get("gates", []):
        results[f"gate:{gate.get('id', 'unknown')}"] = gate.get("status", "unknown")
    for phase in receipt.get("phases", []):
        results[f"phase:{phase.get('id', 'unknown')}"] = phase.get("status", "unknown")
    return results


def compare_baseline(current: dict[str, Any], baseline: dict[str, Any] | None) -> dict[str, Any]:
    if baseline is None:
        return {"available": False, "classifications": {}}
    previous = result_map(baseline)
    classifications: dict[str, str] = {}
    for key, status in result_map(current).items():
        old = previous.get(key)
        if old is None:
            classification = "new-check"
        elif old == "pass" and status == "fail":
            classification = "new-failure"
        elif old == "fail" and status == "fail":
            classification = "inherited-failure"
        elif old == "fail" and status == "pass":
            classification = "fixed"
        else:
            classification = f"unchanged-{status}"
        classifications[key] = classification
    return {
        "available": True,
        "baseline_run_id": baseline.get("run_id"),
        "classifications": classifications,
    }


def host_port_from_ready_url(host: dict[str, Any]) -> int:
    from urllib.parse import urlparse

    parsed = urlparse(host["ready_url"])
    if parsed.hostname not in {"127.0.0.1", "localhost"}:
        raise ExperimentError("manifest.host.ready_url must use loopback")
    if parsed.port is None:
        raise ExperimentError("manifest.host.ready_url must include an explicit port")
    return parsed.port


def run_experiment(
    repo_root: Path,
    manifest_path: Path,
    manifest: dict[str, Any],
    output_root: Path,
    explicit_baseline: Path | None,
) -> tuple[int, Path]:
    run_started = time.monotonic()
    output_root.mkdir(parents=True, exist_ok=True)
    prefix = f"{manifest['id']}-{dt.datetime.now().strftime('%Y%m%dT%H%M%S')}-"
    run_dir = Path(tempfile.mkdtemp(prefix=prefix, dir=output_root))
    state_dir = run_dir / "state"
    state_dir.mkdir()
    receipt_path = run_dir / "receipt.json"
    latest_path = output_root / f"{manifest['id']}-latest.json"
    baseline_path = explicit_baseline or (latest_path if latest_path.exists() else None)
    baseline = read_json(baseline_path) if baseline_path is not None else None
    receipt: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "run_id": run_dir.name,
        "experiment_id": manifest["id"],
        "manifest": str(manifest_path),
        "started_at": utc_now(),
        "status": "running",
        "run_dir": str(run_dir),
        "state_dir": str(state_dir),
        "isolated_state_created": True,
        "gates": [],
        "host_starts": [],
        "phases": [],
    }
    if manifest.get("host") is not None:
        receipt["host_log"] = str(run_dir / "host.log")
    host_process: subprocess.Popen[str] | None = None
    owned_browser = False
    failure: str | None = None

    try:
        for gate in (gate for gate in manifest.get("gates", []) if gate["phase"] == "before"):
            gate_log = run_dir / f"gate-before-{gate['id']}.log"
            result = run_logged_command(gate["command"], repo_root, gate_log)
            result.update({"id": gate["id"], "phase": "before", "required": gate["required"]})
            receipt["gates"].append(result)
            if gate["required"] and result["status"] != "pass":
                raise ExperimentError(f"required before gate failed: {gate['id']}")

        host = manifest.get("host")
        if host is not None:
            port = host_port_from_ready_url(host)
            if port_is_open(port):
                raise ExperimentError(
                    f"host port {port} is already in use; the runner will not reuse or terminate an unowned host"
                )
            context = {"repo_root": repo_root, "run_dir": run_dir, "state_dir": state_dir, "phase_dir": run_dir}
            host_process, host_receipt = start_host(repo_root, host, context, run_dir / "host.log")
            receipt["host_starts"].append(host_receipt)

        owned_browser, browser_receipt = start_browser(manifest["browser"], repo_root, run_dir / "browser-start.log")
        receipt["browser"] = browser_receipt

        for index, phase in enumerate(manifest["phases"]):
            if phase["restart_host"]:
                if host is None:
                    raise ExperimentError(f"phase {phase['id']} requested a host restart without a host")
                stop_receipt = stop_process(host_process, host["shutdown_timeout_ms"])
                if stop_receipt is not None:
                    receipt.setdefault("host_stops", []).append(stop_receipt)
                host_process = None
                context = {
                    "repo_root": repo_root,
                    "run_dir": run_dir,
                    "state_dir": state_dir,
                    "phase_dir": run_dir / f"phase-{index + 1:02d}-{phase['id']}",
                }
                host_process, host_receipt = start_host(repo_root, host, context, run_dir / "host.log")
                receipt["host_starts"].append(host_receipt)
            phase_dir = run_dir / f"phase-{index + 1:02d}-{phase['id']}"
            phase_receipt = run_phase(repo_root, manifest["browser"], phase, phase_dir)
            receipt["phases"].append(phase_receipt)
            write_json(receipt_path, receipt)
            if phase_receipt["status"] != "pass":
                raise ExperimentError(f"browser phase failed: {phase['id']}")

    except (ExperimentError, OSError, subprocess.SubprocessError) as error:
        failure = str(error)
    finally:
        host = manifest.get("host")
        if host is not None:
            stop_receipt = stop_process(host_process, host["shutdown_timeout_ms"])
            if stop_receipt is not None:
                receipt.setdefault("host_stops", []).append(stop_receipt)
        if owned_browser:
            receipt["browser_stop"] = stop_browser(
                manifest["browser"], repo_root, run_dir / "browser-stop.log"
            )
        for gate in (gate for gate in manifest.get("gates", []) if gate["phase"] == "after"):
            gate_log = run_dir / f"gate-after-{gate['id']}.log"
            result = run_logged_command(gate["command"], repo_root, gate_log)
            result.update({"id": gate["id"], "phase": "after", "required": gate["required"]})
            receipt["gates"].append(result)
            if gate["required"] and result["status"] != "pass" and failure is None:
                failure = f"required after gate failed: {gate['id']}"

        receipt["status"] = "fail" if failure else "pass"
        receipt["error"] = failure
        receipt["completed_at"] = utc_now()
        receipt["total_ms"] = elapsed_ms(run_started)
        receipt["baseline"] = compare_baseline(receipt, baseline)
        write_json(receipt_path, receipt)
        write_json(latest_path, receipt)

    return (0 if receipt["status"] == "pass" else 1), receipt_path


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run a real, receipt-backed Atlas browser experiment")
    parser.add_argument("command", nargs="?", choices=("run", "validate"), default="run")
    parser.add_argument("--manifest", default=DEFAULT_MANIFEST)
    parser.add_argument("--output-root", default=".local/atlas-experiment")
    parser.add_argument("--baseline", help="prior receipt used to classify new and inherited failures")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    repo_root = Path(__file__).resolve().parents[2]
    try:
        manifest_path = resolve_repo_path(repo_root, args.manifest, "--manifest")
        manifest = read_json(manifest_path)
        validate_manifest(repo_root, manifest)
        if args.command == "validate":
            print(json.dumps({"status": "valid", "manifest": str(manifest_path)}, sort_keys=True))
            return 0
        output_root = resolve_repo_path(repo_root, args.output_root, "--output-root", must_exist=False)
        baseline_path = (
            resolve_repo_path(repo_root, args.baseline, "--baseline") if args.baseline else None
        )
        status, receipt_path = run_experiment(
            repo_root, manifest_path, manifest, output_root, baseline_path
        )
        receipt = read_json(receipt_path)
        print(
            json.dumps(
                {
                    "status": receipt["status"],
                    "experiment_id": receipt["experiment_id"],
                    "total_ms": receipt["total_ms"],
                    "receipt": str(receipt_path),
                    "error": receipt.get("error"),
                    "baseline": receipt.get("baseline"),
                },
                sort_keys=True,
            )
        )
        return status
    except ExperimentError as error:
        print(json.dumps({"status": "invalid", "error": str(error)}, sort_keys=True), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
