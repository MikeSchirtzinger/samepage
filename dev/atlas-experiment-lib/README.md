# Atlas experiment runner

`dev/atlas-experiment` owns the repeatable lifecycle around a real browser experiment. It creates isolated run state, refuses to take over an occupied host port, starts and stops its host, pins one Chrome tab, captures page and Worker diagnostics through CDP, runs deterministic phase drivers, captures screenshots, and writes one receipt even on failure.

Run the real MobileSAM positive control:

```bash
dev/atlas-experiment
```

Validate a manifest without starting anything:

```bash
dev/atlas-experiment validate --manifest dev/atlas-experiment-lib/experiments/visual-instinct-mobilesam.json
```

Run the trusted-input instrument control:

```bash
dev/atlas-experiment --manifest dev/atlas-experiment-lib/experiments/trusted-range-control.json
```

The trusted-range control proves only that the CDP instrument produces a trusted browser event. It is a fixture, not Atlas evidence.

The checked-in runtime-exception control must fail. It proves that an uncaught page exception cannot coexist with a green phase:

```bash
dev/atlas-experiment --manifest dev/atlas-experiment-lib/experiments/runtime-exception-control.json
```

## Contract

An experiment manifest supplies the variable part:

- an optional host command and loopback readiness URL;
- explicit browser start and stop commands;
- deterministic before and after gates;
- one or more phase drivers, each with checked-in input;
- a `restart_host` boundary for cold-restart persistence checks.

The runner owns the invariant part:

- isolated state and artifact directories;
- host and Chrome ownership tracking;
- pinned-tab isolation;
- direct CDP console, Worker, exception, and network diagnostics;
- trusted click and range primitives;
- screenshots before and after significant phase actions;
- fail-closed cleanup and a receipt on pass or failure;
- comparison with the prior receipt, including `new-failure`, `inherited-failure`, `fixed`, and unchanged classifications.

The default baseline is the prior `<experiment-id>-latest.json` receipt under `.local/atlas-experiment`. Pass `--baseline` to compare against a specific receipt.

## Model boundary

This lifecycle is deterministic. A fast tool-calling model may select a checked-in manifest or draft a new manifest for review. It may not change assertions, authorize actions, suppress diagnostics, or decide whether a gate passes while the experiment is running.

The MobileSAM manifest duplicates expected receipt values intentionally. The host remains the typed source of the browser manifest, while the experiment manifest is an independent assertion surface. A mismatch fails instead of silently updating both sides. Shared contract generation for Atlas integration remains separate work.
