#!/usr/bin/env bash
# Build the samepage-explain wasm artifact, call every export from Node with the
# shared cases, and compare each envelope with the native capability.
#
#   WASM_BINDGEN  wasm-bindgen CLI matching the locked crate in Cargo.lock
#   OUT           output directory (default: a temp dir)
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../../.." && pwd)"
bindgen="${WASM_BINDGEN:-wasm-bindgen}"
out="${OUT:-$(mktemp -d)}"
want="wasm-bindgen $(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/version = |"/, ""); print; exit }' "$root/Cargo.lock")"
[[ "$("$bindgen" --version)" == "$want" ]] || { echo "need $want to match Cargo.lock" >&2; exit 1; }
cd "$root"
cargo build --locked -p samepage-explain --release --target wasm32-unknown-unknown
target="${CARGO_TARGET_DIR:-$root/target}"
artifact="$target/wasm32-unknown-unknown/release/samepage_explain.wasm"
shasum -a 256 "$artifact"
"$bindgen" --target nodejs --out-dir "$out/pkg" "$artifact"
node "$here/check.mjs" "$out/pkg" "$here/cases.json" "$out/results.json"
SAMEPAGE_EXPLAIN_WASM_RESULTS="$out/results.json" cargo test --locked -p samepage-explain --test wasm_equivalence -- --ignored --nocapture
