#!/usr/bin/env bash
# Build the fixture capability for wasm32, generate wasm-bindgen's Node glue,
# and call every export from Node. Proves the export mechanics of
# `export_capability!`; it is not a product capability.
#
#   WASM_BINDGEN  wasm-bindgen CLI matching the pinned crate (0.2.126)
#   OUT           output directory for the generated package (default: a temp dir)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bindgen="${WASM_BINDGEN:-wasm-bindgen}"
out="${OUT:-$(mktemp -d)}"

version="$("$bindgen" --version)"
if [[ "$version" != "wasm-bindgen 0.2.126" ]]; then
  echo "need wasm-bindgen 0.2.126 to match the pinned crate, found: $version" >&2
  exit 1
fi

cd "$here"
cargo build --locked --release --target wasm32-unknown-unknown
target="${CARGO_TARGET_DIR:-$here/target}"
artifact="$target/wasm32-unknown-unknown/release/samepage_capability_fixture.wasm"
shasum -a 256 "$artifact"
"$bindgen" --target nodejs --out-dir "$out" "$artifact"
node "$here/check.mjs" "$out" "$out/results.json"

# The same calls, answered natively, must be byte-identical.
SAMEPAGE_FIXTURE_WASM_RESULTS="$out/results.json" \
  cargo test --locked --test equivalence -- --ignored --nocapture
