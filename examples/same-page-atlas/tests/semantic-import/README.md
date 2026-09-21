# Atlas semantic import learning tests

This is a bounded product learning test during roadmap Phase 2. It does not
open Phase 3 package extraction or Phase 4 runtime governance.

## Human product flow

Choose **Add mask** in the canvas toolbar whenever you want to select an
object. The product path is:

1. A trusted human pointer adds the first positive prompt.
2. The browser Worker returns the real MobileSAM mask and its pinned receipt.
3. Optional positive and negative points refine the same mask from the cached
   image embedding. Clicking or double-clicking an existing point removes it.
4. **Create object** calls `accept_segment` on the browser's WASM CRDT peer.
5. The shared core mints one stable ID, validates and attaches generation 1,
   and removes the proposal if validation fails.
6. **Add part mask** starts a part series under the selected root. Accepted
   masks stay highlighted on the source image, the picker remains on that
   root, and a numbered name advances for the next part.
7. **Finish parts** returns to the root. The Objects tree preserves that
   one-level grouping. Moving a root moves its parts by the same delta.
8. Shift-click selects an exact subset of parts in the Objects tree.
   **Describe motion...** carries that set into chat, where the agent can write
   one bounded keyframe program with translation, rotation, scale, opacity,
   timing, easing, stagger, loops, and alternating direction.
9. The shared core allows a root to target itself and its direct materialized
   parts, or a part to target itself. It refuses cross-object targets,
   duplicate targets, invalid keyframe order, and out-of-range values without
   a partial write.
10. Direct canvas drag and Object control write names, movement, wing pose, and
    motion through that same peer. Motion never rewrites saved x/y positions.
    Reloaded state and `atlas_read` must preserve the object ID, mask,
    hierarchy, human authorship, position, and motion program.

Double-clicking empty canvas opens the ordinary node editor. Clicking away
commits nonempty text and creates nothing when the field is empty.

The visible interaction is the product surface. Worker timings and receipts
remain available under `window.atlas.segment.picker()` for diagnosis and proof,
not as steps the user must operate.

The oracle positive control splits authority deliberately:

1. An MCP agent supplies the semantic label, tags, OCR associations, and one
   source-pixel box through `atlas_segment_oracle_import`.
2. Atlas mints the stable object ID before any mask exists.
3. The browser peer runs the pinned MobileSAM encoder and decoder in a WebGPU
   worker. No alternate execution provider is configured.
4. The browser writes the validated RLE alpha mask and machine receipt through
   the shared CRDT replica.
5. Human pointer movement and agent actions address only the Atlas ID.
6. `atlas_segment_regenerate` applies a real opaque source-pixel occlusion and
   attaches generation 2 to the same ID.

The next experiment changes only step 1. A real multimodal agent inspects the
pinned source and calls `atlas_segment_agent_import`. Its receipt names the
provider, model, request, image digest, exact semantic-output digest, and
no-mock/no-fallback result. Atlas validates every field it can observe before
minting the ID. The segmentation, identity, rendering, movement,
regeneration, persistence, and replica checks remain unchanged.

**The external model invocation is participant-claimed, not host-attested.**
The checked receipt can prove the source and semantic output were bound to the
Atlas write. It cannot prove that a remote provider executed. Preserve the
session evidence for that claim and do not upgrade it to host-verified.

MobileSAM's `iou_predictions` value is a raw regression output, not a bounded
probability. Preserve the finite value verbatim, including a value slightly
above one, and record `predictedIoUContract` in the worker receipt. Silent
clamping would rewrite the measured evidence.

Prepare the pinned model and browser runtime once:

```sh
examples/same-page-atlas/tests/semantic-import/prepare-mobilesam.sh
```

The pure picker mapping and payload contract is checked with:

```sh
node examples/same-page-atlas/tests/semantic-import/test-segment-picker.mjs
```

Build and run Atlas, open it in real Chrome, attach an MCP client, and execute
either import, browser drag, agent move, and occluded regeneration. Then verify
the live host and browser replicas with the matching receipt:

```sh
ATLAS_BROWSER_TAB_ID=<tab-id> \
  examples/same-page-atlas/tests/semantic-import/verify-live.sh

ATLAS_BROWSER_TAB_ID=<tab-id> \
  examples/same-page-atlas/tests/semantic-import/verify-agent-live.sh
```

The measured 2026-08-25 control is recorded in
`2026-08-25-oracle-import-receipt.json`. Its screenshot paths point to local
run evidence and are not substitutes for opening the current surface.

The measured multimodal substitution is recorded separately in
`2026-08-25-multimodal-agent-import-receipt.json`. Never copy control values
into it. The agent-selected semantic fields, prompt box, model provenance,
object ID, worker measurements, gestures, and screenshot hashes must come from
that run.

The measured human product flow is recorded in
`2026-08-26-human-product-flow-receipt.json`. It covers one trusted click,
real mask creation, a trusted direct drag, a trusted animation control, cold
restart persistence, host and browser replica equality, and semantic read-back.

The measured grouped-object follow-up is recorded in
`2026-08-26-object-parts-product-flow-receipt.json`. It covers explicit picker
re-entry, point removal, one root with a selected mask part, grouped movement,
rename persistence, bounded 2D pivot control, click-away node editing, and a
cold restart.

The measured expressive-motion follow-up is recorded in
`2026-08-26-expressive-owl-motion-receipt.json`. It covers trusted multi-part
selection, the prompt handoff, exact agent targets, mirrored wing tracks,
staggered claws, stationary feet, an illegal cross-object target, offline
write refusal, and cold restarts of both disposable and live state.

## Repeatable Evidence Loop experiment

Run the complete generated-infographic flow with one command:

```sh
dev/atlas-evidence-loop
```

The runner owns an isolated state directory, the Same Page Atlas host, a
pinned real Chrome tab, direct CDP diagnostics, screenshots, cleanup, and a
baseline-aware receipt. Its blocking gates regenerate the browser WASM peer
and check the generated segmentation catalog before the browser opens.

The first browser phase selects the Evidence Loop source, accepts five trusted
MobileSAM masks as one root and four parts, and verifies the pinned WebGPU
Worker receipts. It then uses authenticated MCP to store a clockwise loop with
staggered stage pulses. A deliberately invalid target must fail without
changing that program. Six sampled frames must show all five animations
running and changing. The second phase restarts the real host and requires the
same IDs, masks, hierarchy, motion program, visible animations, and exact
host/browser read-back equality.

Run the same command again. A repeatable result reports every gate and both
phases as `unchanged-pass`. The measured 2026-08-26 run is summarized in
`2026-08-26-evidence-loop-product-flow-receipt.json`.

MobileSAM is class-agnostic. These receipts prove real mask computation and
the selected regions, not ground-truth semantic accuracy. The human labels
`Observe`, `Segment`, `Act`, and `Verify` remain human-authored meaning.

**Mocks and fallback execution are failures.** Missing or drifted assets stop
server startup. A stale generation, wrong graph digest, malformed RLE, CPU
substitution, copied receipt, or browser-only DOM object does not pass.
