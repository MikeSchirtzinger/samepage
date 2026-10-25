#!/usr/bin/env bash
# Build doc.wasm, generate wasm-bindgen's Node glue, run the facade check, and
# (optionally) have the native host admit a frame the wasm replica produced.
#
#   WASM_BINDGEN  wasm-bindgen CLI matching the locked crate in Cargo.lock
#   OUT           output directory (default: a temp dir)
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
bindgen="${WASM_BINDGEN:-wasm-bindgen}"
out="${OUT:-$(mktemp -d)}"
want="wasm-bindgen $(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/version = |"/, ""); print; exit }' "$root/Cargo.lock")"
[[ "$("$bindgen" --version)" == "$want" ]] || { echo "need $want to match Cargo.lock" >&2; exit 1; }
cd "$root"
cargo build --locked -p samepage-doc-web --release --target wasm32-unknown-unknown
target="${CARGO_TARGET_DIR:-$root/target}"
artifact="$target/wasm32-unknown-unknown/release/samepage_doc_web.wasm"
shasum -a 256 "$artifact"
"$bindgen" --target nodejs --out-dir "$out/pkg" "$artifact"
node "$here/check.mjs" "$out/pkg" "$out/frame.bin"
SAMEPAGE_DOC_WASM_FRAME="$out/frame.bin" cargo test --locked -p samepage-doc --test wasm_frame -- --ignored
