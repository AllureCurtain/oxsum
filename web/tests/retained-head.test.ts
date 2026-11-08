import assert from "node:assert/strict";
import test from "node:test";
import { archiveKey, checkHead, readArchive, retainHead, type HeadVerifier, type SignedHead } from "../src/lib/retained-head.ts";

const identity = { site: "https://billing.example", organizationId: "org-a", tenantId: "tenant-a" };
const served: SignedHead = { origin: "oxsum/ledgers/tenant-a", size: 1, root: "ab".repeat(32), note: "signed", keyName: "operator", keyHash: "12345678", publicKey: "ab".repeat(32) };
const pass: HeadVerifier = { verifySignedHead: () => '{"ok":true}', verifyConsistency: () => '{"ok":true}' };
const noFetch = async () => { throw new Error("unexpected network call"); };

test("archives are isolated by site, customer and ledger and reject legacy/malformed records", async () => {
  const held = await retainHead(identity, served, pass);
  for (const changed of [{ ...identity, site: "https://other.example" }, { ...identity, organizationId: "other" }, { ...identity, tenantId: "other" }]) {
    assert.notEqual(archiveKey(identity), archiveKey(changed));
    assert.throws(() => readArchive(JSON.stringify(held), changed), /another customer/);
  }
  assert.throws(() => readArchive(JSON.stringify({ size: 1, root: served.root }), identity), /invalid/);
  assert.throws(() => readArchive("{broken", identity));
  assert.equal(readArchive(null, identity), null);
  assert.deepEqual(readArchive(JSON.stringify(held), identity), held);
});

test("retention requires a real positive verdict and exact customer origin", async () => {
  for (const answer of ['{"ok":false}', '{"ok":"true"}', '{}', 'null', 'not-json']) {
    await assert.rejects(retainHead(identity, served, { ...pass, verifySignedHead: () => answer }));
  }
  await assert.rejects(retainHead(identity, served, { ...pass, verifySignedHead: () => { throw new Error("WASM unavailable"); } }), /WASM unavailable/);
  await assert.rejects(retainHead(identity, { ...served, origin: "oxsum/ledgers/other" }, pass), /another customer/);
  for (const size of [-1, 0.5, Number.MAX_SAFE_INTEGER + 1]) await assert.rejects(retainHead(identity, { ...served, size }, pass), /Invalid ledger/);
});

test("network success cannot bypass consistency verification or substitute the retained key", async () => {
  const held = await retainHead(identity, served, pass);
  const next = { ...served, size: 2 };
  const response = { ...next, oldHead: held.head, head: { size: 2, root: next.root }, proof: { oldSize: 1, newSize: 2, path: [] } };
  await assert.rejects(checkHead(held, next, { ...pass, verifyConsistency: () => '{"ok":false}' }, async () => response), /did not confirm/);
  await assert.rejects(checkHead(held, { ...next, publicKey: "cd".repeat(32) }, pass, noFetch), /key changed/);
  await assert.rejects(checkHead(held, { ...served, size: 0 }, pass, noFetch), /smaller/);
  await assert.rejects(checkHead(held, { ...served, root: "cd".repeat(32) }, pass, noFetch), /different root/);
  assert.equal((await checkHead(held, served, pass, noFetch)).kind, "unchanged");
  assert.equal(held.head.size, 1);
});

test("empty heads can be retained and signature-checked without claiming historical continuity", async () => {
  const held = await retainHead(identity, { ...served, size: 0 }, pass);
  assert.equal((await checkHead(held, served, pass, noFetch)).kind, "emptyAnchor");
});


test("a changing consistency response cannot replace the head observed during the check", async () => {
  const held = await retainHead(identity, served, pass);
  const current = { ...served, size: 2 };
  const response = { ...current, oldHead: held.head, head: { size: 3, root: served.root }, proof: { oldSize: 1, newSize: 3, path: [] } };
  await assert.rejects(checkHead(held, current, pass, async () => response), /differs from the observed head/);
});
