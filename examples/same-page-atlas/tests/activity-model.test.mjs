import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
const source = await readFile(new URL("../static/activity-model.js", import.meta.url), "utf8");
const { wasmSpans, hostSpans, layoutSpans } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);

test("missing timing stays absent and recorded host phases keep their parent", () => {
  const event = { kind:"action.read", actor:{label:"agent"}, correlation:{eventId:"e1",runId:"r1"}, outcome:"succeeded" };
  assert.deepEqual(hostSpans({events:[event]}), []);
  const spans = hostSpans({events:[{...event,timing:{startedAtMs:1000,durationUs:3000,spans:[{name:"execute_effect",offsetUs:500,durationUs:2500}]}}]});
  assert.equal(spans[1].parent, spans[0].id);
  assert.equal(spans[1].start,1000.5);
  assert.equal(spans[1].duration,2.5);
  const layout = layoutSpans(spans);
  assert.equal(layout.spans.find(s=>s.parent).lane,1);
  assert.equal(layout.duration,3);
});

test("evicted openings are disclosed and durations come from recorded monotonic elapsed time", () => {
  const spans = wasmSpans({records:[{kind:"span_close",span_id:2,parent_span_id:1,name:"read",at_ms:100,duration_ms:4,trace_id:1}]});
  assert.equal(spans[0].partial,true);
  assert.equal(spans[0].duration,4);
  assert.equal(spans[0].start,96);
});

test("nested siblings share a lane only when their measured intervals do not overlap", () => {
  const spans = [{id:"r",parent:null,start:0,duration:20},{id:"a",parent:"r",start:1,duration:5},{id:"b",parent:"r",start:7,duration:4},{id:"c",parent:"r",start:3,duration:5}];
  const layout = layoutSpans(spans);
  const get = id=>layout.spans.find(s=>s.id===id);
  assert.equal(get("a").lane,get("b").lane);
  assert.notEqual(get("a").lane,get("c").lane);
});

test("zero-resolution durations remain zero instead of being reported as invented elapsed time", () => {
  const layout = layoutSpans([{id:"r",parent:null,start:42,duration:0}]);
  assert.equal(layout.duration,0);
  assert.equal(layout.spans[0].duration,0);
  assert.ok(Number.isFinite(layout.spans[0].width));
});
