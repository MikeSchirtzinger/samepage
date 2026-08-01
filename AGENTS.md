# Overview

AG-UI Rust is an agent-first application platform. The canonical sequencing
authority is `docs/platform-roadmap.md`; Govern is a derived coordination and
evidence index, not a replacement roadmap. Phase 2, agent-driven setup, is the
only active roadmap phase.

# Agent navigation

- Read `agent-harness.json` for the machine-readable map of authorities,
  resource roots, progressive guides, commands, extension mechanics, and proof
  vocabulary.
- Read `docs/agent-harness/README.md` for task routing. Load the complete
  environment, extension, same-page, decision, or Govern guide only when the
  task needs it.
- Run `python3 dev/agent-harness/check.py --json` when current resource
  provenance, recipe selection, branch state, or documentation diagnostics
  matter.
- For rationale, use `docs/decisions/index.json` and the indexed record. If no
  rationale is documented, say `unknown` or `undocumented`; do not reconstruct
  intent from source code alone.

# Architecture

The runtime kernel owns actions, events, state, context, providers,
interruption, human decisions, evidence, and transport-neutral dispatch.
Browser WASM and local WASI are distinct hosts for the same portable component
contract. Example applications prove compositions; they do not define generic
platform vocabulary.

# Boundaries

- Keep Phase 3, Phase 4, and Phase 5 work parked until their entry gates are
  explicitly opened in `docs/platform-roadmap.md`.
- One bounded pre-Phase-4 `clear_board` learning test may exercise Govern
  around a real runtime action, but it must not become a runtime dependency.
- A mock, fallback response, disabled feature, or receipt copied from another
  path does not count as proof.
- Preserve separate claims for implemented, compiled, tested, opened in real
  Chrome, and proven by a machine-readable receipt.
- Clean setup may not depend on an undocumented sibling checkout.
- `crates/convergence-attrs` is a compatibility shim, not enforcement.
- Do not couple the runtime kernel to the Govern CLI, its store, or its
  extractor. `dev/govern` is development coordination only.

# Commands

- `python3 dev/agent-harness/check.py` validates the checked-in agent resource
  graph, decision index, and Same Page Studio extension assets. It is
  configuration evidence, not compiler or browser proof.
- `dev/govern/bootstrap.sh` initializes and refreshes the local Govern index.
- `dev/govern/check.sh` reports roadmap drift and obligations without blocking.
- `dev/govern/check.sh --enforce` applies the same checks as a blocking gate.
  This is the **export-boundary gate**: run it (green) before curating a
  concern out to the `~/dev/ag-ui` storefront fork. In the lab itself the
  checks stay advisory so iteration is never blocked.
- `dev/govern/spec-drift-check.sh` diffs `ag-ui-core`'s `EventType` against
  live upstream AG-UI; the `.github/workflows/spec-drift` job runs it weekly
  and fails loudly on any drift. `AGUI-EVENTTYPE-SHAPE-01` +
  `AGUI-CONFORMANCE-SUITE-01` gate the same conformance in `--enforce`.
- Follow the phase-specific deterministic build and real-surface proof commands
  documented by the roadmap and component-host packages.

# Testing

Validate changes at the real layer they affect. Rust compiler and test success
prove code paths, while browser claims require real Chrome and cross-host claims
require the checked-in machine-readable receipt. Never widen a gate or add a
fallback merely to make a check pass.

# Decisions

- `docs/platform-roadmap.md` remains the single sequencing authority.
- Phase 2 is active; later phases remain parked.
- Govern is being dogfooded now as an advisory development control plane.
- Per-run enforcement is allowed for clean-checkout probes and future CI, but
  repository adoption does not change a developer's global Govern policy.
- Runtime preflight and postcondition governance remains a Phase 4 concern.

# Parked Ideas

- Phase 3 packages: Extract generic packages and multi-language SDKs after the Phase 2 exit gate.
- Phase 4 runtime governance: Add preflight, postconditions, evals, and governed action receipts after the Phase 3 exit gate.
- Phase 5 P2P: Add scoped invitations, revocation, and reconnect behavior after the Phase 4 exit gate.
