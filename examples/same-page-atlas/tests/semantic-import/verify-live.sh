#!/bin/sh
set -eu

atlas_dir=$(CDPATH='' cd "$(dirname "$0")/../.." && pwd)
receipt="$atlas_dir/tests/semantic-import/2026-08-25-oracle-import-receipt.json"
url=${AGUI_ATLAS_URL:-http://127.0.0.1:8098}
tab=${ATLAS_BROWSER_TAB_ID:-}
expected_id=$(jq -r '.object.id' "$receipt")
expected_source=$(jq -r '.source.sha256' "$receipt")
expected_encoder=$(jq -r '.generation_1.encoder_sha256' "$receipt")
expected_decoder=$(jq -r '.generation_1.decoder_sha256' "$receipt")

host=$(curl -fsS "$url/atlas/segments")
printf '%s' "$host" | jq -e \
  --arg id "$expected_id" \
  --arg source "$expected_source" \
  --arg encoder "$expected_encoder" \
  --arg decoder "$expected_decoder" '
    .ok == true and
    (.segments | length) == 1 and
    .segments[0].id == $id and
    .segments[0].segment_generation == 2 and
    .segments[0].segment_status == "materialized" and
    .segments[0].segment_source_sha256 == $source and
    .segments[0].segment_encoder_sha256 == $encoder and
    .segments[0].segment_decoder_sha256 == $decoder and
    .segments[0].segment_receipt.stableObjectId == $id and
    .segments[0].segment_receipt.executionLocation == "browser-worker" and
    .segments[0].segment_receipt.backendRequested == "webgpu" and
    .segments[0].segment_receipt.alternateProviderConfigured == false and
    .segments[0].segment_receipt.occlusionApplied == true and
    .segments[0].segment_receipt.fallbackUsed == false and
    .segments[0].segment_receipt.mockUsed == false
  ' >/dev/null

if [ -z "$tab" ]; then
  printf '%s\n' "ATLAS_BROWSER_TAB_ID is required for replica proof" >&2
  exit 1
fi

browser=$(browser-eval --tab "$tab" \
  'JSON.stringify(window.atlas.segment.snapshot().map(({id,segment_generation,segment_status,segment_source_sha256,segment_encoder_sha256,segment_decoder_sha256,segment_occlusion_box})=>({id,segment_generation,segment_status,segment_source_sha256,segment_encoder_sha256,segment_decoder_sha256,segment_occlusion_box})))')
host_projection=$(printf '%s' "$host" | jq -cS \
  '[.segments[] | {id,segment_generation,segment_status,segment_source_sha256,segment_encoder_sha256,segment_decoder_sha256,segment_occlusion_box}]')
replicas_equal=$(jq -n --argjson host "$host_projection" --argjson browser "$browser" '$host == $browser')
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

printf '%s\n' "ATLAS_ORACLE_IMPORT_PASS id=$expected_id generation=2 replicas=equal"
