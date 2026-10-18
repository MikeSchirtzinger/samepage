// Focused graph control; real WASM/network behavior is checked in Chrome.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const source = readFileSync(new URL('../capability-loader.js', import.meta.url));
const { wasmDependencyClosure } = await import(`data:text/javascript;base64,${source.toString('base64')}`);

function node(id, version, depends_on, native = false) {
  const descriptor = { depends_on };
  return [id, native
    ? { id, version, nativeOnly: descriptor }
    : { id, version, artifact: { descriptor } }];
}

test('a NativeOnly adapter exposes its transitive WASM dependency in order', () => {
  const byId = new Map([
    node('pins', '1', []),
    node('tags', '1', [{ id: 'pins', version: '1' }]),
    node('adapter', '1', [{ id: 'tags', version: '1' }], true),
    node('spare', '1', [
      { id: 'adapter', version: '1' },
      { id: 'pins', version: '1' },
    ]),
  ]);
  assert.deepEqual(wasmDependencyClosure('spare', byId), ['pins', 'tags']);
  assert.deepEqual(wasmDependencyClosure('adapter', byId), ['pins', 'tags']);
});

test('an unavailable or version-mismatched dependency refuses the closure', () => {
  const byId = new Map([
    node('adapter', '1', [{ id: 'tags', version: '2' }], true),
    node('tags', '1', []),
    node('spare', '1', [{ id: 'adapter', version: '1' }]),
  ]);
  assert.throws(() => wasmDependencyClosure('spare', byId), /dependency tags@2 is unavailable/);
  byId.delete('tags');
  assert.throws(() => wasmDependencyClosure('spare', byId), /dependency tags@2 is unavailable/);
});
