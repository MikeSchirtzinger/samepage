#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../../.." && pwd)"
url="${1:-http://127.0.0.1:8101}"
work="$(mktemp -d /tmp/same-page-wasm-trace-proof.XXXXXX)"
started_browser=0

cleanup() {
  browser-nav about:blank >/dev/null 2>&1 || true
  if [[ "$started_browser" == "1" ]]; then
    browser-stop >/dev/null 2>&1 || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT

if ! curl -fsS --max-time 2 http://127.0.0.1:9222/json/version >"$work/browser-version.json"; then
  browser-start >/dev/null
  started_browser=1
  curl -fsS --max-time 5 http://127.0.0.1:9222/json/version >"$work/browser-version.json"
fi

eval "$(browser-tab)"
browser-nav "$url" >"$work/navigation.txt"

browser-eval "new Promise((resolve) => {
  const deadline = Date.now() + 15000;
  const tick = () => {
    const view = document.getElementById('atlas-view');
    const traceApi = typeof globalThis.__samePageAtlasTrace;
    if (document.readyState === 'complete' && view && view.children.length && traceApi === 'object') {
      resolve({
        ready: true,
        title: document.title,
        view_children: view.children.length,
        trace_api: traceApi,
      });
    } else if (Date.now() > deadline) {
      resolve({
        ready: false,
        title: document.title,
        view_children: view?.children.length || 0,
        trace_api: traceApi,
      });
    } else {
      setTimeout(tick, 50);
    }
  };
  tick();
})" >"$work/ready.json"

browser-eval "(() => {
  const api = globalThis.__samePageAtlasTrace;
  api.clear();
  performance.clearMarks();
  performance.clearMeasures();
  const snapshot_trace = api.captureDocument();
  const measures = performance.getEntriesByType('measure').map(({ name, duration, startTime }) => ({
    name,
    duration,
    startTime,
  }));
  const benchmark = api.benchmark(1024, 5);
  const overflow_trace = api.snapshot();
  const view = document.getElementById('atlas-view');
  const browser = {
    ready: document.readyState === 'complete',
    title: document.title,
    view_children: view?.children.length || 0,
    trace_api: typeof api,
  };
  performance.clearMarks();
  performance.clearMeasures();
  return { snapshot_trace, performance: { measures }, benchmark, overflow_trace, browser };
})()" >"$work/probe.json"

browser-screenshot "$here/latest.png" >"$work/screenshot.txt"

bun="${BUN_BIN:-bun}"
if ! command -v "$bun" >/dev/null 2>&1; then
  echo "bun is required (install it, or set BUN_BIN to its path): https://bun.sh" >&2
  exit 1
fi

cd "$repo"
"$bun" "$here/write-receipt.mjs" \
  "$work/ready.json" \
  "$work/probe.json" \
  "$work/browser-version.json" \
  "$here/latest.png" \
  "$url" \
  "$here/latest.json"

printf 'live trace receipt: %s\n' "$here/latest.json"
