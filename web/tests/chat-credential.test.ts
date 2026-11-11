import assert from "node:assert/strict";
import test from "node:test";
import { createChatCredentialStore, requireChatIdentity, type ChatIdentity, type ChatKey } from "../src/lib/chat-credential.ts";
const a = { organizationId: "customer-a", userId: "alice" };
const b = { organizationId: "customer-b", userId: "bob" };
function setup() {
  const storage = new Map<string, string>();
  const state = { identity: a, keys: [] as ChatKey[], minted: 0, wait: null as Promise<void> | null };
  const owners = new Map<string, string>();
  const store = createChatCredentialStore({
    storage: { getItem: (key) => storage.get(key) ?? null, setItem: (key, value) => { storage.set(key, value); } },
    session: async () => state.identity,
    keys: async () => [...state.keys],
    keyOrganization: async (secret) => owners.get(secret) ?? "",
    now: () => Date.parse("2026-11-10T00:00:00Z"),
    mint: async () => {
      const current = state.identity;
      state.minted += 1;
      const id = `key-${state.minted}`, secret = `secret-${id}`;
      const key = { id, name: "chat", prefix: secret.slice(0, 8), createdBy: current.userId, revokedAt: null, expiresAt: null };
      owners.set(secret, current.organizationId);
      if (state.wait) await state.wait;
      state.keys.push(key);
      return { ...key, secret };
    },
  });
  return { state, storage, owners, store };
}
test("A/B/A and another user never reuse another principal's secret", async () => {
  const { state, storage, store } = setup();
  storage.set("oxsum.chat-key", "legacy-b-secret");
  const first = await store.ensure(a);
  state.identity = b; const second = await store.ensure(b);
  state.identity = a; const again = await store.ensure(a);
  assert.equal(again.secret, first.secret); assert.notEqual(second.secret, first.secret);
  const other = { ...a, userId: "carol" }; state.identity = other;
  const third = await store.ensure(other); assert.notEqual(third.secret, first.secret);
  assert.equal(state.minted, 3);
});
test("revoked, expired, deleted and foreign credentials do not authorize reuse", async () => {
  for (const failure of ["revoked", "expired", "deleted", "foreign"]) {
    const { state, storage, owners, store } = setup();
    const first = await store.ensure(a);
    if (failure === "revoked") state.keys[0].revokedAt = "2026-11-09T00:00:00Z";
    if (failure === "expired") state.keys[0].expiresAt = "2026-11-09T00:00:00Z";
    if (failure === "deleted") state.keys = [];
    if (failure === "foreign") owners.set(first.secret, b.organizationId);
    const next = await store.ensure(a); assert.notEqual(next.secret, first.secret);
    assert.equal(JSON.parse(storage.get(`oxsum.chat-key.v2:${a.organizationId}:${a.userId}`)!).keyId, next.keyId);
  }
});
test("effect replay shares one pending creation", async () => {
  const { state, store } = setup(); let release!: () => void;
  state.wait = new Promise<void>((resolve) => { release = resolve; });
  const first = store.ensure(a), second = store.ensure(a); assert.equal(first, second);
  release(); assert.deepEqual(await first, await second); assert.equal(state.minted, 1);
});
test("late creation after account switch is rejected and never stored", async () => {
  const { state, storage, store } = setup(); let release!: () => void;
  state.wait = new Promise<void>((resolve) => { release = resolve; });
  const pending = store.ensure(a);
  while (!state.minted) await new Promise((resolve) => setImmediate(resolve));
  state.identity = b; release(); await assert.rejects(pending, /active account changed/);
  assert.equal(storage.size, 0);
});
test("current session mismatch refuses before key lookup or creation", async () => {
  const { state, store } = setup(); state.identity = b;
  await assert.rejects(store.ensure(a), /active account changed/); assert.equal(state.minted, 0);
  await assert.rejects(requireChatIdentity(a, async (): Promise<ChatIdentity> => b), /active account changed/);
});
