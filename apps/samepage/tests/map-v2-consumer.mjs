import assert from 'node:assert/strict';
import test from 'node:test';

class Element {
  constructor(tag) {
    this.tag = tag;
    this.children = [];
    this.dataset = {};
    this.handlers = new Map();
    this.style = {setProperty() {}};
    this.classList = {add() {}};
    this.clientWidth = 800;
    this.clientHeight = 600;
    this.hidden = true;
    this.textContent = '';
  }
  appendChild(child) { this.children.push(child); return child; }
  replaceChildren(...children) { this.children = children; }
  addEventListener(name, handler) { this.handlers.set(name, handler); }
  removeEventListener(name) { this.handlers.delete(name); }
  setAttribute() {}
  setPointerCapture() {}
}

test('Map sends the capability v2 planning Batch to applyBatch', async () => {
  const mount = new Element('main');
  const count = new Element('span');
  const alert = new Element('div');
  const cards = [];
  globalThis.document = {
    getElementById(id) { return id === 'map-count' ? count : alert; },
    createElement(tag) {
      const element = new Element(tag);
      if (tag === 'article') cards.push(element);
      return element;
    },
    createElementNS(_namespace, tag) { return new Element(tag); },
    createDocumentFragment() { return new Element('fragment'); },
  };

  const batch = {base_revision: 'r1', entries: [{capability: 'map', plan: {ops: [], readback: ''}}]};
  let renderer;
  let applied;
  const application = {
    registerCapability(id, wasm) { assert.equal(id, 'map'); assert.equal(wasm.id, 'wasm-v2'); },
    registerRenderer(id, value) { assert.equal(id, 'map'); renderer = value; return () => {}; },
    project: async () => ({cards: [{id: 'c1', label: 'Card', x: 10, y: 20}], links: []}),
    plan: async (id, action, args) => {
      assert.equal(id, 'map');
      assert.equal(action, 'card_move');
      assert.deepEqual(args, {id: 'c1', x: 19, y: 31});
      return batch;
    },
    apply() { throw new Error('bare apply must not receive a v2 planning Batch'); },
    async applyBatch(value) { applied = value; },
    ready: () => true,
  };

  const {activate} = await import(new URL('../modules/map.js', import.meta.url));
  activate({mount, wasm: {id: 'wasm-v2'}, application});
  await renderer.render({
    view: {revision: 'r1', records: []},
    revision: 'r1',
  });
  assert.equal(cards.length, 1);
  const card = cards[0];
  card.handlers.get('pointerdown')({button: 0, clientX: 100, clientY: 100, pointerId: 1, preventDefault() {}});
  await card.handlers.get('pointerup')({clientX: 109, clientY: 111});
  assert.deepEqual(applied, batch);
  assert.equal(alert.hidden, true);
});
