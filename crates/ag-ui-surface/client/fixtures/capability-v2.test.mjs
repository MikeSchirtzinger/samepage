import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

globalThis.location = new URL('http://127.0.0.1:8190/');
const source = readFileSync(new URL('../capability-loader.js', import.meta.url));
const {
  CapabilityLoader,
  triggeredCapabilityIdsForView,
  validateCapabilityManifest,
} = await import(`data:text/javascript;base64,${source.toString('base64')}`);

const hash = 'a'.repeat(64);
const file = (url) => ({url, sha256: hash, bytes: 1});

function extension(id, triggers, {kinds = [], props = [], reads = [], actions = []} = {}) {
  const descriptor = {
    abi: 'samepage.capability.v2', id, version: '1', schema: 1,
    artifact: {type: 'wasm'}, kinds, props, reads, depends_on: [],
    actions, visual_actions: [], triggers,
  };
  return {
    id, version: '1', loading: 'on-demand', module: `/capabilities/${id}/index.js`,
    actions: actions.filter((action) => action.audience !== 'agent').map((action) => action.name),
    events: [], capabilities: [], required: true, mount: `${id}-mount`,
    artifact: {
      kind: 'capability', abi: 'samepage.capability.v2', descriptor,
      entry: file(`/capabilities/${id}/index.js`),
      glue: file(`/capabilities/${id}/${id}.js`),
      wasm: file(`/capabilities/${id}/${id}_bg.wasm`),
    },
  };
}

function manifest(extensions) {
  return {
    protocol: 'ag-ui', schemaVersion: 1, capabilitySchemaVersion: 1,
    document: {
      pkgName: 'samepage_doc_web', js: '/capabilities/doc/doc.js',
      jsSha256: hash, jsBytes: 1, wasm: '/capabilities/doc/doc_bg.wasm',
      sha256: hash, bytes: 1, abi: 'samepage.document.v1', layoutVersion: 1,
    },
    extensions,
  };
}

const diagramAction = {
  name: 'diagram_draw', aliases: [], audience: 'both', effect: 'mutate',
  description: 'draw', input_schema: {type: 'object'},
};
const edgeClaim = {kind: 'edge', keys: ['event'], reason: 'typed transitions'};

test('v2 accepts property metadata, an unrelated tool name, and the root route', () => {
  const diagram = extension('diagram', [
    {on: 'tool', name: 'diagram'},
    {on: 'route', name: '/'},
    {on: 'property', kind: 'edge', key: 'event'},
  ], {kinds: [edgeClaim], actions: [diagramAction]});
  assert.equal(validateCapabilityManifest(manifest([diagram])).extensions[0].id, 'diagram');
});

test('v1 and invalid property trigger metadata are refused', () => {
  const old = extension('old', [{on: 'always'}]);
  old.artifact.abi = 'samepage.capability.v1';
  old.artifact.descriptor.abi = 'samepage.capability.v1';
  assert.throws(() => validateCapabilityManifest(manifest([old])), /invalid capability artifact/);

  const wrongKey = extension('diagram', [
    {on: 'property', kind: 'edge', key: 'guard'},
  ], {kinds: [edgeClaim]});
  assert.throws(() => validateCapabilityManifest(manifest([wrongKey])), /edge\.guard is neither owned nor extended/);

  const wrongShape = extension('diagram', [
    {on: 'property', kind: 'edge', key: 'event', name: 'event'},
  ], {kinds: [edgeClaim]});
  assert.throws(() => validateCapabilityManifest(manifest([wrongShape])), /invalid property trigger/);
});

test('property activation observes key presence across initial and changed views', async () => {
  const ordinary = extension('ordinary', [{on: 'kind', name: 'edge'}], {reads: ['edge']});
  const diagram = extension('diagram', [
    {on: 'property', kind: 'edge', key: 'event'},
    {on: 'property', kind: 'edge', key: 'event'},
  ], {kinds: [edgeClaim]});
  const extensions = [ordinary, diagram];
  const empty = {revision: 'empty', records: []};
  const plain = {revision: 'plain', records: [{id: 'e1', kind: 'edge', props: {label: 'plain'}}]};
  const typed = {revision: 'typed', records: [{id: 'e1', kind: 'edge', props: {event: ''}}]};

  assert.deepEqual(triggeredCapabilityIdsForView(extensions, empty), []);
  assert.deepEqual(triggeredCapabilityIdsForView(extensions, plain), ['ordinary']);
  assert.deepEqual(triggeredCapabilityIdsForView(extensions, typed), ['ordinary', 'diagram']);

  const loader = new CapabilityLoader(manifest(extensions));
  const activated = [];
  loader.activate = async (id) => { activated.push(id); return id; };
  await loader.activateForView(empty, () => null);
  await loader.activateForView(plain, () => null);
  await loader.activateForView(typed, () => null);
  assert.deepEqual(activated, ['ordinary', 'ordinary', 'diagram']);
});
