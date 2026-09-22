import assert from "node:assert/strict";
import test from "node:test";

import {
  beginPersistenceHandshake,
  finishPersistenceHandshake,
  persistenceFailed,
  persistencePending,
  persistencePresentation,
} from "../web/persistence.mjs";

test("a healthy reconnect clears only the failure it carried in", () => {
  const failed = persistenceFailed({failure:null, pending:null, generation:0}, "old host failed");
  const start = beginPersistenceHandshake(failed);
  assert.deepEqual(finishPersistenceHandshake(failed, start), {
    failure: null,
    pending: null,
    generation: 1,
  });
});

test("a failure between hello frames survives the ready transition", () => {
  const healthy = {failure:null, pending:null, generation:0};
  const start = beginPersistenceHandshake(healthy);
  const failedDuringHandshake = persistenceFailed(healthy, "new host failed");
  const retained = finishPersistenceHandshake(failedDuringHandshake, start);
  assert.strictEqual(retained, failedDuringHandshake);
  assert.deepEqual(persistencePresentation(retained), {
    status: "Save failed",
    banner: "Document persistence failed: new host failed",
  });
});

test("a newer failure cannot be erased while clearing an older one", () => {
  const oldFailure = persistenceFailed({failure:null, pending:null, generation:0}, "old failure");
  const start = beginPersistenceHandshake(oldFailure);
  const newFailure = persistenceFailed(oldFailure, "new failure between hello frames");
  assert.strictEqual(finishPersistenceHandshake(newFailure, start), newFailure);
});

test("a new pending save during handshake is retained", () => {
  const healthy = {failure:null, pending:null, generation:0};
  const start = beginPersistenceHandshake(healthy);
  const pending = persistencePending(healthy, "commit 7 is still saving");
  assert.strictEqual(finishPersistenceHandshake(pending, start), pending);
});
