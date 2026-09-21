#!/usr/bin/env bun

import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { relative, resolve } from "node:path";

const [readyPath, probePath, browserVersionPath, screenshotPath, url, outputPath] = process.argv.slice(2);
if (!outputPath) {
  throw new Error("expected ready, probe, browser version, screenshot, URL, and output paths");
}

const repo = resolve(import.meta.dirname, "../../../..");
const wasmPath = resolve(repo, "examples/same-page-atlas/web/pkg/same_page_atlas_web_bg.wasm");
const wasm = readFileSync(wasmPath);
const screenshot = readFileSync(screenshotPath);
const ready = JSON.parse(readFileSync(readyPath, "utf8"));
const probe = JSON.parse(readFileSync(probePath, "utf8"));
const browserVersion = JSON.parse(readFileSync(browserVersionPath, "utf8"));

if (!ready.ready || !probe.browser?.ready) {
  throw new Error("Same Page Atlas did not reach a healthy browser state");
}

const baseline = {
  wasm_bytes: 1194613,
  wasm_sha256: "ace7329814ba164aabc296f16a28706d8c9eea4a54ea7790f38bb30a4de39a92",
  command: "examples/same-page-atlas/build-web.sh, then stat and shasum -a 256",
};
const candidateBytes = wasm.length;
const receipt = {
  schema: "same-page-wasm-trace-receipt-v1",
  measured_at: new Date().toISOString(),
  baseline,
  candidate: {
    wasm_bytes: candidateBytes,
    wasm_sha256: createHash("sha256").update(wasm).digest("hex"),
    delta_bytes: candidateBytes - baseline.wasm_bytes,
  },
  browser: {
    version: browserVersion.Browser,
    url,
    ready: probe.browser.ready,
    title: probe.browser.title,
    view_children: probe.browser.view_children,
    trace_api: probe.browser.trace_api,
  },
  snapshot_trace: probe.snapshot_trace,
  performance: probe.performance,
  benchmark: probe.benchmark,
  overflow_trace: probe.overflow_trace,
  screenshot: {
    path: relative(repo, screenshotPath),
    sha256: createHash("sha256").update(screenshot).digest("hex"),
  },
};

writeFileSync(outputPath, `${JSON.stringify(receipt, null, 2)}\n`);
console.log(JSON.stringify({
  candidate_bytes: candidateBytes,
  delta_bytes: receipt.candidate.delta_bytes,
  snapshot_records: receipt.snapshot_trace.records.length,
  overflow_emitted: receipt.overflow_trace.emitted,
  overflow_dropped: receipt.overflow_trace.dropped,
  browser: receipt.browser.version,
}));
