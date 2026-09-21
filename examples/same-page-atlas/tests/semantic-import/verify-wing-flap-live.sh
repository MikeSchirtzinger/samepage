#!/bin/sh
set -eu

atlas_dir=$(CDPATH='' cd "$(dirname "$0")/../.." && pwd)
receipt="$atlas_dir/tests/semantic-import/2026-08-25-wing-flap-receipt.json"
url=${AGUI_ATLAS_URL:-http://127.0.0.1:8098}
tab=${ATLAS_BROWSER_TAB_ID:-}
expected_id=$(jq -r '.object.id' "$receipt")

if [ -z "$tab" ]; then
  printf '%s\n' "ATLAS_BROWSER_TAB_ID is required for browser proof" >&2
  exit 1
fi

host=$(curl -fsS "$url/atlas/segments")
printf '%s' "$host" | jq -e --arg id "$expected_id" '
  .ok == true and
  (.segments | length) == 1 and
  .segments[0].id == $id and
  .segments[0].segment_generation == 3 and
  .segments[0].segment_status == "materialized" and
  .segments[0].segment_flap == 1 and
  .segments[0].touched_by == "human" and
  (.segments[0].segment_parts | length) == 2 and
  [.segments[0].segment_parts[].role] == ["left-wing", "right-wing"] and
  ([.segments[0].segment_parts[].receipt.backendRequested] | all(. == "webgpu")) and
  ([.segments[0].segment_parts[].receipt.fallbackUsed] | all(. == false)) and
  ([.segments[0].segment_parts[].receipt.mockUsed] | all(. == false)) and
  .segments[0].segment_receipt.backendRequested == "webgpu" and
  .segments[0].segment_receipt.partCount == 2 and
  .segments[0].segment_receipt.fallbackUsed == false and
  .segments[0].segment_receipt.mockUsed == false
' >/dev/null

browser=$(browser-eval --tab "$tab" '
  JSON.stringify({
    snapshot: window.atlas.segment.snapshot().map((shape) => ({
      id: shape.id,
      segment_generation: shape.segment_generation,
      segment_status: shape.segment_status,
      segment_flap: shape.segment_flap,
      touched_by: shape.touched_by,
      roles: shape.segment_parts.map((part) => part.role),
    })),
    rendered: window.atlas.segment.rendered(),
    events: window.atlas.segment.inputEvents(),
    schema: window.atlas.schema,
  })
')
printf '%s' "$browser" | jq -e --arg id "$expected_id" '
  .snapshot == [{
    id: $id,
    segment_generation: 3,
    segment_status: "materialized",
    segment_flap: 1,
    touched_by: "human",
    roles: ["left-wing", "right-wing"]
  }] and
  (.rendered | length) == 1 and
  .rendered[0].canvas.maskPainted == "true" and
  .rendered[0].canvas.wingsSubtracted == "2" and
  .rendered[0].slider.visible == true and
  .rendered[0].slider.value == "1" and
  [.rendered[0].wings[].role] == ["left-wing", "right-wing"] and
  [.rendered[0].wings[].maskPainted] == ["true", "true"] and
  [.rendered[0].wings[].transform] == ["rotate(35deg)", "rotate(-35deg)"] and
  (.events | length) > 0 and
  ([.events[].trusted] | all(. == true)) and
  .schema == {host: 6, wasm: 6, ready: true}
' >/dev/null

host_projection=$(printf '%s' "$host" | jq -cS '[.segments[] | {
  id,
  segment_generation,
  segment_status,
  segment_flap,
  touched_by,
  roles: [.segment_parts[].role]
}]')
browser_projection=$(printf '%s' "$browser" | jq -cS '.snapshot')
replicas_equal=$(jq -n \
  --argjson host "$host_projection" \
  --argjson browser "$browser_projection" \
  '$host == $browser')
if [ "$replicas_equal" != true ]; then
  printf '%s\n' "structured browser and host replicas differ" >&2
  exit 1
fi

host_text=$(curl -fsS "$url/atlas/describe" | jq -r '.text')
browser_text=$(browser-eval --tab "$tab" 'window.atlas.doc.describe()')
if [ "$host_text" != "$browser_text" ]; then
  printf '%s\n' "browser and host descriptions differ" >&2
  exit 1
fi

printf '%s\n' "ATLAS_WING_FLAP_PASS id=$expected_id generation=3 flap=1 attribution=human replicas=equal"
