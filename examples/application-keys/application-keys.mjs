#!/usr/bin/env node
/** An already scoped application manages a bounded customer key without owner login.
 * Uses an explicit existing application-owned customer; never invokes a model or adds funds. */
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";

const base = new URL(process.env.OXSUM_BASE_URL ?? "http://127.0.0.1:3000");
const token = process.env.OXSUM_APPLICATION_CREDENTIAL;
const customer = process.env.OXSUM_APPLICATION_CUSTOMER_ID;
const model = process.env.OXSUM_APPLICATION_MODEL;
if (!token || !customer || !model || !/^[0-9a-f-]{36}$/i.test(customer)) throw new Error("Set the application credential, customer ID and allowed model.");
if (base.username || base.password || base.search || base.hash || (base.protocol !== "https:" && !(base.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(base.hostname)))) throw new Error("Use HTTPS or an isolated loopback listener, with no URL credentials.");
async function call(path, body) {
  const response = await fetch(new URL(`/api/v1${path}`, base), { method: body ? "POST" : "GET", headers: { authorization: `Bearer ${token}`, ...(body ? { "content-type": "application/json" } : {}) }, body: body ? JSON.stringify(body) : undefined, redirect: "error", signal: AbortSignal.timeout(10000) });
  if (!response.ok) throw new Error(`Application key request failed (HTTP ${response.status})`);
  return (await response.json()).data;
}
const path = `/integration/customers/${customer}/keys`;
const statePath = process.env.OXSUM_APPLICATION_KEY_STATE;
if (!statePath) throw new Error("Set OXSUM_APPLICATION_KEY_STATE to a new task-owned state file; keep it for retry/recovery.");
let state;
try {
  state = JSON.parse(await readFile(statePath, "utf8"));
} catch (error) {
  if (error.code !== "ENOENT") throw error;
  state = { version: 1, base: base.origin, customer, body: { idempotencyKey: randomUUID(), name: "Application walkthrough", reason: "bounded key lifecycle acceptance", expiresAt: new Date(Date.now() + 300000).toISOString(), spendLimitMinor: 1000, modelAllowlist: [model], requestsPerMinute: 2, maxConcurrentHolds: 1 }, revoke: { idempotencyKey: randomUUID(), reason: "clean up application key walkthrough" } };
  await writeFile(statePath, JSON.stringify(state, null, 2) + "\n", { flag: "wx", mode: 0o600 });
}
if (state.version !== 1 || state.base !== base.origin || state.customer !== customer || state.body?.modelAllowlist?.[0] !== model || !state.body?.idempotencyKey || !state.revoke?.idempotencyKey) throw new Error("The retained operation belongs to different inputs; preserve it for its original recovery.");
if (state.closed) {
  console.log("PASS: this retained walkthrough operation is already revoked");
} else {
  let created;
  try {
    created = await call(path, state.body);
  } catch {
    // One bounded retry uses exactly the durable request, including its expiry.
    created = await call(path, state.body);
  }
  try {
    const replay = await call(path, state.body);
    assert.equal(replay.key.id, created.key.id);
    assert.equal(replay.secret, null);
    const page = await call(`${path}?limit=100`);
    assert(Array.isArray(page.keys));
    assert(page.keys.every((key) => !("secret" in key)));
    console.log("PASS: bounded issuance, exact metadata-only replay and secret-free listing");
  } finally {
    await call(`${path}/${created.key.id}/revoke`, state.revoke);
    state.closed = true;
    state.keyId = created.key.id;
    await writeFile(statePath, JSON.stringify(state, null, 2) + "\n", { mode: 0o600 });
    console.log("PASS: issued key revoked");
  }
}
