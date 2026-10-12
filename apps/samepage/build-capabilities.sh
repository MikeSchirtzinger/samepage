#!/usr/bin/env bash
# Build only the selected document and capability packages. No Atlas web bundle.
# Usage: build-capabilities.sh OUT_DIR doc=crates/samepage-doc/web \
#          map=crates/samepage-map=apps/samepage/modules/map.js ...
# Every pure capability requires a real, supplied entry module; none is generated.
set -euo pipefail

if (( $# < 3 )); then
  echo 'usage: build-capabilities.sh OUT_DIR doc=CRATE capability=CRATE=ENTRY ...' >&2
  exit 2
fi
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
out="$1"
shift
case "$out" in /*) ;; *) echo 'OUT_DIR must be absolute' >&2; exit 2;; esac
mkdir -p "$out"
out="$(cd "$out" && pwd -P)"
if [[ "$out" == "$root" || ( "$out" == "$root/"* && "$out" != "$root/.local/"* ) ]]; then
  echo 'OUT_DIR must be outside the source checkout or inside its ignored .local directory' >&2; exit 2
fi
if [[ "${CARGO_BUILD_JOBS:-4}" -gt 4 ]]; then
  echo 'CARGO_BUILD_JOBS must be at most 4 for this lane' >&2; exit 2
fi
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo 'set a private CARGO_TARGET_DIR before building' >&2; exit 2
fi
command -v wasm-pack >/dev/null
command -v wasm-opt >/dev/null
command -v python3 >/dev/null
command -v node >/dev/null

staging="$(mktemp -d "${out%/}/.stage.XXXXXX")"
trap 'status=$?; rm -rf "$staging"; exit $status' EXIT
seen='|'
found_doc=0
integrated_sources=0
for selection in "$@"; do
  if [[ "$selection" == sources=* ]]; then integrated_sources=1; fi
done
for spec in "$@"; do
  IFS='=' read -r id crate entry extra <<< "$spec"
  if [[ -n "${extra:-}" || ! "$id" =~ ^[a-z][a-z0-9_-]*$ || -z "$crate" ]]; then
    echo "invalid selection: $spec" >&2; exit 2
  fi
  if [[ "$seen" == *"|$id|"* ]]; then echo "duplicate selection: $id" >&2; exit 2; fi
  seen+="$id|"
  if [[ "$crate" == /* || "$crate" == *..* || ! -f "$root/$crate/Cargo.toml" ]]; then
    echo "selected crate is not in this checkout: $crate" >&2; exit 2
  fi
  if [[ "$id" == doc ]]; then
    (( found_doc == 0 )) || { echo 'document selected twice' >&2; exit 2; }
    found_doc=1
    [[ -z "${entry:-}" ]] || { echo 'document has no entry module' >&2; exit 2; }
  else
    if [[ -z "${entry:-}" || "$entry" == /* || "$entry" == *..* || ! -f "$root/$entry" ]]; then
      echo "capability $id needs a real checkout entry module" >&2; exit 2
    fi
    if rg -i 'same[-_]page[-_]atlas|segmentation|agent[-_]ink' "$root/$entry" >/dev/null; then
      echo "capability $id entry references an excluded source or artifact" >&2; exit 2
    fi
  fi
  cargo metadata --locked --format-version 1 --filter-platform wasm32-unknown-unknown \
    --manifest-path "$root/$crate/Cargo.toml" > "$staging/$id.metadata.json"
  python3 - "$staging/$id.metadata.json" "$root/$crate/Cargo.toml" <<'PY'
import json, pathlib, sys
graph = json.load(open(sys.argv[1]))
manifest = pathlib.Path(sys.argv[2]).resolve()
packages = {p['id']: p for p in graph['packages']}
nodes = {n['id']: n for n in graph['resolve']['nodes']}
roots = [p['id'] for p in packages.values() if pathlib.Path(p['manifest_path']).resolve() == manifest]
if len(roots) != 1: raise SystemExit(f'cannot identify selected crate in Cargo resolve: {manifest}')
seen, pending = set(), roots
while pending:
    pkg_id = pending.pop()
    if pkg_id in seen: continue
    seen.add(pkg_id)
    pkg = packages[pkg_id]
    name = pkg['name'].replace('_', '-').lower()
    path = pkg['manifest_path'].replace('\\', '/').lower()
    if name.startswith('same-page-atlas') or 'segmentation' in name or 'agent-ink' in name or '/examples/same-page-atlas/' in path:
        raise SystemExit(f'excluded product dependency in selected WASM closure: {pkg["name"]} {pkg["manifest_path"]}')
    for dep in nodes[pkg_id]['deps']:
        if any(kind.get('kind') != 'dev' for kind in dep['dep_kinds']):
            pending.append(dep['pkg'])
print(f'checked selected WASM Cargo closure: {pkg["name"] if len(seen)==1 else len(seen)} packages')
PY
  mkdir -p "$staging/$id"
  extra_features=()
  if [[ "$id" == diagrams && "$integrated_sources" == 1 ]]; then extra_features=(--features integrated-sources); fi
  wasm-pack build "$root/$crate" --release --target web --out-dir "$staging/$id" --no-typescript -- --locked ${extra_features[@]+"${extra_features[@]}"}
  shopt -s nullglob
  glues=("$staging/$id/"*_bg.wasm)
  scripts=("$staging/$id/"*.js)
  if (( ${#glues[@]} != 1 || ${#scripts[@]} != 1 )); then
    echo "expected one wasm and one glue JS for $id" >&2; exit 1
  fi
  optimized="$staging/$id/optimized.wasm"
  wasm-opt --enable-bulk-memory --enable-nontrapping-float-to-int -Oz "${glues[0]}" -o "$optimized"
  mv "$optimized" "${glues[0]}"
  node --input-type=module - "$id" "${scripts[0]}" "${glues[0]}" "$staging/$id/identity.json" <<'JS'
import fs from 'node:fs';
import { pathToFileURL } from 'node:url';
const [id, glue, wasm, receipt] = process.argv.slice(2);
const module = await import(pathToFileURL(glue).href);
await module.default({ module_or_path: new Uint8Array(fs.readFileSync(wasm)) });
const expected = id === 'doc' ? 'samepage.document.v1' : 'samepage.capability.v2';
if (module.abi?.() !== expected) throw Error(`${id}: built WASM exported ABI mismatch`);
const identity = { abi: expected };
if (id === 'doc') {
  if (module.layout_version?.() !== 1) throw Error('doc: built WASM layout version mismatch');
  identity.layoutVersion = 1;
} else {
  const envelope = typeof module.descriptor === 'function' ? JSON.parse(module.descriptor()) : null;
  if (envelope?.abi !== expected || envelope?.error || envelope?.ok?.id !== id ||
      envelope.ok.abi !== expected || envelope.ok.artifact?.type !== 'wasm') {
    throw Error(`${id}: built WASM descriptor identity mismatch`);
  }
  identity.descriptor = envelope.ok;
}
fs.writeFileSync(receipt, JSON.stringify(identity));
JS
  if [[ "$id" != doc ]]; then cp "$root/$entry" "$staging/$id/index.js"; fi
  # Optional capability-local styling travels only with the selected package.
  if [[ "$id" != doc && -f "$root/${entry%.js}.css" ]]; then
    cp "$root/${entry%.js}.css" "$staging/$id/style.css"
  fi
  shopt -u nullglob
done
if (( found_doc != 1 )); then echo 'one document package is required' >&2; exit 2; fi

python3 - "$staging" "$out" "$root" <<'PY'
import hashlib, json, os, pathlib, sys
stage, out, root = map(pathlib.Path, sys.argv[1:])
artifacts = []
for directory in sorted(p for p in stage.iterdir() if p.is_dir()):
    glue = [p for p in directory.glob('*.js') if p.name != 'index.js']
    wasm = list(directory.glob('*_bg.wasm'))
    if len(glue) != 1 or len(wasm) != 1:
        raise SystemExit(f'incomplete package: {directory.name}')
    def identity(path):
        data = path.read_bytes()
        return {'url': '/capabilities/' + directory.name + '/' + path.name,
                'sha256': hashlib.sha256(data).hexdigest(), 'bytes': len(data)}
    item = {'id': directory.name, 'kind': 'document' if directory.name == 'doc' else 'capability',
            'abi': 'samepage.document.v1' if directory.name == 'doc' else 'samepage.capability.v2',
            'glue': identity(glue[0]), 'wasm': identity(wasm[0])}
    observed = json.loads((directory / 'identity.json').read_text())
    if directory.name == 'doc': item['layoutVersion'] = observed['layoutVersion']
    else:
        item['entry'] = identity(directory / 'index.js')
        item['descriptor'] = observed['descriptor']
    artifacts.append(item)
manifest = {'schemaVersion': 1, 'artifacts': artifacts}
(stage / 'build-owner').write_text('samepage-capabilities.v1\n')
for item in artifacts:
    (stage / item['id'] / '.samepage-build-owned').write_text(item['id'] + '\n')
manifest['buildOwner'] = 'samepage-capabilities.v1'
(stage / 'capabilities.json').write_text(json.dumps(manifest, sort_keys=True, separators=(',', ':')) + '\n')
PY
python3 - "$staging" "$out" <<'PY'
import json, os, pathlib, shutil, sys
stage, out = map(pathlib.Path, sys.argv[1:])
new = json.loads((stage / 'capabilities.json').read_text())
old_path = out / 'capabilities.json'
if old_path.exists():
    old = json.loads(old_path.read_text())
    if old.get('buildOwner') != 'samepage-capabilities.v1':
        raise SystemExit('refusing to replace an output root without a C3-owned manifest')
else:
    old = {'artifacts': []}
old_ids = {item['id'] for item in old['artifacts']}
new_ids = {item['id'] for item in new['artifacts']}
for id in old_ids | new_ids:
    target = out / id
    if target.exists() or target.is_symlink():
        if target.is_symlink() or not target.is_dir() or (target / '.samepage-build-owned').read_text() != id + '\n':
            raise SystemExit(f'refusing to replace unowned output: {target}')
for id in old_ids | new_ids:
    target = out / id
    if target.exists(): shutil.rmtree(target)
for id in new_ids:
    os.replace(stage / id, out / id)
os.replace(stage / 'capabilities.json', old_path)
PY
trap - EXIT
rm -rf "$staging"
