# Evaluation layers

AG-UI uses three separate evaluation layers. They answer different questions,
run against different evidence, and must never be collapsed into one green
label.

| Layer | Question | Model | Evidence |
|---|---|---|---|
| **CONFORMANCE** | Does a protocol, host, or capability obey its deterministic contract? | None | Compiler/test output and deterministic receipts |
| **AGENT EVAL** | Can a real provider/model still produce one named extension capability outcome on the real surface? | Required | Host-owned state or action receipt after the fixed scenario |
| **RUN SCORING** | How did sampled real sessions perform after the fact? | Optional scorer, never the acting session | Versioned sample metadata, source receipts, and post-hoc scorer results |

The runner lives in `crates/ag-ui-eval`. It is an external proof producer. The
runtime kernel does not depend on it, its files, or its results. This follows
ADR 0006's checker boundary and does not introduce Phase 4 preflight,
postconditions, or runtime governance. Those broader Phase 4 concerns remain
separate.

## Fail-closed statuses

The receipt uses literal, separate statuses:

| Status | Meaning | Exit |
|---|---|---:|
| `pass` | Every discovered scenario executed and every scorer met its threshold | 0 |
| `fail` | An execution failed or at least one scorer missed its threshold | 1 |
| `configuration_error` | The suite is missing, empty, malformed, duplicated, or has no valid scorer | 2 |
| `skipped` | Every scenario was unavailable for a stated reason | 3 |
| `incomplete` | Some scenarios passed and at least one was skipped with a reason | 3 |

A skip is not a pass. A missing provider executable, credential, or reachable
model can make an **AGENT EVAL** scenario `skipped`, but the suite exits 3. A
skip exit without a nonempty reason in JSON becomes `fail`. **CONFORMANCE**
cannot convert command failures into skips.

A missing suite path and a directory containing no `.json` files always emit a
machine-readable `configuration_error` receipt and exit 2.

## Executable scenario format

The v1 schema is
`specs/evals/schema/scenario-v1.schema.json`. Each scenario names:

- one layer: `conformance` or `agent_eval`;
- one subject id and one atomic capability statement;
- for **AGENT EVAL**, the real adapter, provider, and model;
- one bounded command and its availability requirements;
- one or more observable scorers with explicit thresholds;
- an `exit_code == 0` scorer, which is mandatory.

Supported observations are the process exit code and values selected from the
command's stdout JSON using a JSON Pointer. Supported thresholds are `eq`,
`contains`, `gte`, and `lte`. Any miss makes the scenario and suite fail.

The real per-extension example is
`specs/evals/agent-eval/atlas-companion-place-node.json`. It drives Pi through
OpenRouter using a real model. That model must call the running `atlas`
extension through `/surface/action` as a companion agent. The model's prose is
not the acceptance surface: scorers inspect the host's `/atlas/describe`
read-back and require exactly one node carrying the driver's per-run fixture
label, so a port cannot satisfy the check with one hard-coded receipt.

The existing deterministic example is
`specs/evals/conformance/ag-ui-core-fixture-roundtrip.json`. It classifies
`ag-ui-core`'s existing canonical event-fixture round-trip test as
**CONFORMANCE** without rewriting the test.

Run them from the repository root:

```bash
cargo run -p ag-ui-eval -- run \
  --layer conformance \
  --suite specs/evals/conformance \
  --output artifacts/conformance.json

cargo run -p ag-ui-eval -- run \
  --layer agent_eval \
  --suite specs/evals/agent-eval \
  --output artifacts/agent-eval.json
```

## Gating extension distillation

Before an extension is rewritten, define one scenario per capability that must
survive, using the extension id and an observable end state. Run the fixed
scenario against the original implementation to establish that the gate can
observe the capability. Run the same file against the distilled
implementation. Compilation, module activation, or feature count cannot
substitute for that outcome.

A useful capability statement is narrow enough to fail independently:

```text
atlas: companion agent creates one node through shared dispatch
shared-fields: companion update reaches persisted field read-back
reciprocal-attention: agent point resolves to the host-selected semantic target
```

Do not make one scenario claim that an entire extension “works.” Separate state
mutation, read-back, reconnect, attribution, and other independently failing
behaviors into separate files and scorers.

## RUN SCORING contract

`specs/evals/run-scoring/contract-v1.schema.json` defines the post-hoc receipt.
Every batch records:

- the population, window, sampling method, inclusion rule, sampler version, and
  each session's selection probability;
- an opaque source receipt reference rather than copied or invented evidence;
- extension id and named capabilities;
- the provider/model that produced the sampled session;
- each scorer's observed value, threshold, and recomputed pass state;
- separate pass, fail, and reason-bearing skipped counts.

`specs/evals/run-scoring/example-v1.json` is explicitly illustrative, not a
real-session receipt. The command below validates its contract and emits a
machine-readable contract-validation receipt with
`"sampler_implemented": false`:

```bash
cargo run -p ag-ui-eval -- validate-run-scoring \
  --contract specs/evals/run-scoring/example-v1.json \
  --output artifacts/run-scoring-contract.json
```

Selecting production sessions, retrieving traces, and executing post-hoc
scorers are out of scope for this layer's first landing. Contract validation
must not be reported as sampled-session scoring.

## CI

`.github/workflows/evaluation.yml` runs all three distinct jobs on a nightly or
manual invocation:

- `conformance`;
- `agent-eval-real-provider`;
- `run-scoring-contract`.

Each job uploads its JSON receipt with `if: always()`. The real-provider job is
allowed to go red with exit 3 when the provider is unavailable; its receipt
still states why. The Rust runner has no Node or React dependency. Pi is only
installed in the targeted **AGENT EVAL** job as the selected external provider
adapter.
