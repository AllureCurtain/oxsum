import assert from "node:assert/strict";
import test from "node:test";
import { credentialRequest, type GrantDraft } from "../src/lib/integration-grant.ts";

const draft: GrantDraft = { mode: "application", applicationId: "app-a", customerIds: ["portal-customer"], scopes: ["customersRead", "meteringWrite"], name: "My app", days: "30", reason: "runtime grant" };

test("application grants exclude unrelated customers and freeze exact expiry and identity", () => {
  const mutable = structuredClone(draft);
  const request = credentialRequest(mutable, "operation-a", Date.UTC(2026, 10, 9));
  assert.deepEqual(request.customerIds, []);
  assert.equal(request.applicationId, "app-a");
  assert.equal(request.expiresAt, "2026-12-09T00:00:00.000Z");
  const transmitted = JSON.stringify(request);
  mutable.customerIds.push("later-customer");
  mutable.scopes.push("billingRead");
  assert.equal(JSON.stringify(request), transmitted);
  assert.equal(request.idempotencyKey, "operation-a");
});

test("legacy grant paths cannot inherit an application or its lifecycle scope", () => {
  assert.throws(() => credentialRequest({ ...draft, mode: "customers", scopes: ["customersLifecycleWrite"] }, "key", 0), /scopes/);
  assert.throws(() => credentialRequest({ ...draft, mode: "customers", scopes: ["entitlementsRead"] }, "key", 0), /scopes/);
  assert.deepEqual(credentialRequest({ ...draft, scopes: ["entitlementsRead"] }, "key", 0).scopes, ["entitlementsRead"]);
  assert.throws(() => credentialRequest({ ...draft, mode: "customers", customerIds: [] }, "key", 0), /customers/);
  const provision = credentialRequest({ ...draft, mode: "provision" }, "key", 0);
  assert.equal(provision.applicationId, undefined);
  assert.deepEqual(provision.customerIds, []);
  assert.deepEqual(provision.scopes, ["customersProvision"]);
  const selected = credentialRequest({ ...draft, mode: "customers", customerIds: ["b", "a", "b"] }, "key", 0);
  assert.equal(selected.applicationId, undefined);
  assert.deepEqual(selected.customerIds, ["a", "b"]);
});

test("malformed and out-of-range expiries or missing authority fail before submission", () => {
  for (const days of ["0", "366", "1.5", "1e2", "-1", "NaN"]) assert.throws(() => credentialRequest({ ...draft, days }, "key", 0), /expiry/);
  assert.throws(() => credentialRequest({ ...draft, applicationId: "" }, "key", 0), /application/);
  assert.throws(() => credentialRequest({ ...draft, scopes: [] }, "key", 0), /scopes/);
  assert.throws(() => credentialRequest({ ...draft, reason: " " }, "key", 0), /details/);
});

test("customer key management is confined to application grants", () => {
  assert.throws(() => credentialRequest({ ...draft, mode: "customers", scopes: ["customerKeysWrite"] }, "key", 0), /scopes/);
  const request = credentialRequest({ ...draft, scopes: ["customerKeysWrite"] }, "key", 0);
  assert.equal(request.applicationId, "app-a");
  assert.deepEqual(request.customerIds, []);
  assert.deepEqual(request.scopes, ["customerKeysWrite"]);
});

test("frozen evidence requires an explicit application grant", () => {
  assert.throws(() => credentialRequest({ ...draft, mode: "customers", scopes: ["evidenceRead"] }, "key", 0), /scopes/);
  const request = credentialRequest({ ...draft, scopes: ["evidenceRead"] }, "key", 0);
  assert.equal(request.applicationId, "app-a");
  assert.deepEqual(request.customerIds, []);
  assert.deepEqual(request.scopes, ["evidenceRead"]);
});

test("checkout grant requires an application namespace", () => {
  assert.throws(() => credentialRequest({ mode: "customers", applicationId: "", customerIds: ["customer"], scopes: ["checkoutWrite"], name: "", days: "7", reason: "checkout" }, "key", 0));
});

test("evidence publication is explicit and application-only", () => {
  assert.throws(() => credentialRequest({ ...draft, mode: "customers", scopes: ["evidenceWrite"] }, "key", 0), /scopes/);
  assert.deepEqual(credentialRequest({ ...draft, scopes: ["evidenceWrite"] }, "key", 0).scopes, ["evidenceWrite"]);
});

test("notification metadata requires its own application-only grant", () => {
  assert.throws(() => credentialRequest({ ...draft, mode: "customers", scopes: ["notificationsRead"] }, "key", 0), /scopes/);
  assert.deepEqual(credentialRequest({ ...draft, scopes: ["notificationsRead"] }, "key", 0).scopes, ["notificationsRead"]);
});
