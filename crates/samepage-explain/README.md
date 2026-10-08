# Explain

Explain is an optional ABI v2 capability depending on Map and Sketch. Its nine
WASM exports run the same portable graph validation, state machine, scoped
drawing and read-back as the native implementation. `plan` returns one Batch.
No document or Yrs runtime enters its normal dependency closure.

`walkthrough_define` creates or replaces a named finite graph, retaining its
object id. `walkthrough_advance` and `walkthrough_control` require the observed
cursor revision. `walkthrough_read` returns every flow or one named flow;
`walkthrough_draw` composes Map cards, Sketch shapes and Explain membership
in one commit without rewriting the cursor. Node `ref` names can bind drawn
arrows in the same batch. Legacy explanation names dispatch as hidden aliases.

A flow stores the retained encoded definition and one coherent JSON cursor.
Scoped node/shape membership is an independent `exploration_id` scalar, so
concurrent ink does not overwrite another participant's cursor or scope.
Definitions are agent-only; cursor controls and evidence are shared. The host
authority hook also checks batches and replica frames, including same-value
attempts, and refuses stale or fabricated transitions. Authorship comes from
the host record stamp, never the embedded legacy author text.

The lazy browser module highlights the current step, animates point/reveal,
traces drawn connectors and Map links, exposes button transitions, and submits
free responses with explicitly selected transitions and target evidence.
Pause/resume keep the entered revision; restart re-enters the start step.
Browser animation, selection and draft text remain local. The native adapter
publishes semantic attention only after durable cursor entry.

`replay-motion` is retained by name in the visual registry and reported as
unsupported in the player and read-back. Its removed runtime is never loaded.
Other actions and the rest of the flow keep running.

Run `cargo test --locked -p samepage-explain`, then
`tests/wasm/run-check.sh` for actual native/WASM equivalence.
