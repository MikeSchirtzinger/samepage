#!/usr/bin/env bash
# Shared environment setup; capability selection stays in each thin recipe.
set -euo pipefail
if (( $# < 3 )); then echo 'usage: recipe PROJECT_ROOT [capability selections supplied by the recipe]' >&2; exit 2; fi
recipe="$1"; project="$2"; shift 2
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
project="$(cd "$project" && pwd -P)"
run="${SAMEPAGE_RECIPE_DIR:-$root/.local/recipes/$recipe}"
mkdir -p "$run"
run="$(cd "$run" && pwd -P)"
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
export TMPDIR="${TMPDIR:-$run/tmp}"
mkdir -p "$TMPDIR" "$run/assets/web"
cd "$root"
"$root/apps/samepage/build-capabilities.sh" "$run/capabilities" "$@"
wasm-pack build "$root/crates/ag-ui-wasm-client" --release --target web --out-dir "$run/assets/protocol" --no-typescript -- --locked
cargo build --locked -p samepage-app
cp "$root/apps/samepage/web/"* "$run/assets/web/"
cp "$root/apps/samepage/models.json" "$run/assets/models.json"
# CI builds every recipe artifact without starting the host.
if [[ -n "${SAMEPAGE_RECIPE_BUILD_ONLY:-}" ]]; then echo "built $recipe in $run"; exit 0; fi
export SAMEPAGE_STATE_DIR="${SAMEPAGE_STATE_DIR:-$run/state}"
export SAMEPAGE_CAPABILITIES_DIR="$run/capabilities"
export SAMEPAGE_APP_ASSETS_DIR="$run/assets"
export SAMEPAGE_PROJECT_ROOT="$project"
export SAMEPAGE_ADDR="${SAMEPAGE_ADDR:-127.0.0.1:8190}"
export AGUI_MCP_TOKEN="${AGUI_MCP_TOKEN:-local-samepage-token}"
exec "$CARGO_TARGET_DIR/debug/samepage"
