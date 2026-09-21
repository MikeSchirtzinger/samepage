#!/usr/bin/env bash
# Build the browser's CRDT replica.
#
# The page is a real yrs peer, not a view, so this step is not optional: the
# host refuses to start without it rather than serving a read-only surface that
# looks collaborative.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! command -v wasm-pack >/dev/null 2>&1; then
  echo "wasm-pack is required: cargo install wasm-pack" >&2
  exit 1
fi

# wasm-pack's own bundled wasm-opt predates the bulk-memory ops rustc emits,
# which is why web/Cargo.toml sets `wasm-opt = false` for the release profile.
# The pass runs here instead, against a wasm-opt on PATH that is new enough.
# It is required rather than optional: a build that silently skips it ships a
# different binary than the one anyone measured.
if ! command -v wasm-opt >/dev/null 2>&1; then
  echo "wasm-opt is required: cargo install wasm-opt --locked" >&2
  exit 1
fi

cd "$here/web"
feature_args=()
case "${AGUI_ATLAS_USER_TIMING:-0}" in
  1 | true | on | yes)
    feature_args=(--features user-timing)
    ;;
  0 | false | off | no)
    ;;
  *)
    echo "AGUI_ATLAS_USER_TIMING must be 1/0, true/false, on/off, or yes/no" >&2
    exit 1
    ;;
esac
if [[ "${#feature_args[@]}" -gt 0 ]]; then
  wasm-pack build --release --target web --out-dir pkg . "${feature_args[@]}"
else
  wasm-pack build --release --target web --out-dir pkg
fi

artifact="pkg/same_page_atlas_web_bg.wasm"
before="$(wc -c <"$artifact" | tr -d ' ')"
wasm-opt -Oz \
  --enable-bulk-memory \
  --enable-nontrapping-float-to-int \
  --enable-mutable-globals \
  --enable-sign-ext \
  --enable-reference-types \
  "$artifact" -o "$artifact.opt"
mv "$artifact.opt" "$artifact"
after="$(wc -c <"$artifact" | tr -d ' ')"

echo
echo "wasm-opt -Oz: ${before} bytes to ${after} bytes"
echo "built $here/web/pkg, the runtime serves it at /pkg"
