import assert from "node:assert/strict";
import test from "node:test";
import { servingReadinessStates, type ServingReadiness } from "../src/lib/serving-readiness.ts";

const sample = { keyId: "key", model: "free", prompt: "Hello" as const, maxOutputTokens: 32 as const, inputUpperBound: 9, boundMinor: 0, quotaMinor: 0, walletMinor: 0, saleSource: "default" as const, saleVersion: 1, discountPercent: 0 };
const ready: ServingReadiness = { asOf: "2026-11-09T00:00:00Z", keyReady: true, modelReady: true, fundingReady: true, reason: null, scanLimited: false, sample };
const query = { isPending: false, isError: false };
test("free and fully entitled samples complete without a positive wallet", () => {
  assert.deepEqual(servingReadinessStates({ ...query, data: ready }), { channel: "done", credit: "done", key: "done" });
  assert.equal(servingReadinessStates({ ...query, data: { ...ready, sample: { ...sample, boundMinor: 50, quotaMinor: 50 } } }).credit, "done");
});
test("uncertainty, missing terms and stale keys never become ready", () => {
  for (const reason of ["quotaTransitionRequired", "ledgerUninitialized", "promoExpiryPending", "budgetExceeded"]) {
    assert.equal(servingReadinessStates({ ...query, data: { ...ready, fundingReady: false, reason } }).credit, "todo");
  }
  assert.deepEqual(servingReadinessStates({ ...query, data: { ...ready, keyReady: false, modelReady: false, fundingReady: false, sample: null } }), { channel: "todo", credit: "todo", key: "todo" });
  assert.equal(servingReadinessStates({ ...query, data: { ...ready, sample: null } }).credit, "todo");
});
test("pending, errors and restricted access always resolve explicitly", () => {
  assert.equal(servingReadinessStates({ ...query, isPending: true }).channel, "loading");
  assert.equal(servingReadinessStates({ ...query, isError: true, error: { status: 500 } }).channel, "error");
  assert.equal(servingReadinessStates({ ...query, isError: true, error: { status: 403 } }).channel, "blocked");
  assert.equal(servingReadinessStates({ ...query, data: { ...ready, reason: "restricted" } }).key, "blocked");
  assert.equal(servingReadinessStates(query).credit, "error");
});
