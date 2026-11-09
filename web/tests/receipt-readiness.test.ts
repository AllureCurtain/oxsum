import assert from "node:assert/strict";
import test from "node:test";
import { receiptProofHref, receiptReadinessState } from "../src/lib/receipt-readiness.ts";

const ready = { state: "ready" as const, receipt: { requestId: "request", entryId: "entry", contentHash: "a".repeat(64), model: "free", chargedMinor: 0, walletChargedMinor: 0 } };
const query = { isPending: false, isError: false };

test("zero-cost canonical receipts complete both steps and open the exact entry", () => {
  assert.equal(receiptReadinessState({ ...query, data: ready }), "done");
  assert.equal(receiptProofHref(ready), "/verify?entry=entry");
});
test("incomplete, absent and restricted evidence never looks completed", () => {
  assert.equal(receiptReadinessState({ ...query, data: { state: "missing", receipt: null } }), "todo");
  assert.equal(receiptReadinessState({ ...query, data: { state: "incomplete", receipt: null } }), "error");
  assert.equal(receiptReadinessState({ ...query, data: { state: "ready", receipt: null } }), "error");
  assert.equal(receiptReadinessState({ ...query, isError: true, error: { status: 403 } }), "blocked");
  assert.equal(receiptReadinessState({ ...query, isError: true, error: { status: 500 } }), "error");
  assert.equal(receiptReadinessState({ ...query, isPending: true }), "loading");
  assert.equal(receiptProofHref({ state: "incomplete", receipt: ready.receipt }), undefined);
  assert.equal(receiptProofHref(), undefined);
});
