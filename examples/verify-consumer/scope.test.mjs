import assert from 'node:assert/strict';
import { createHash, generateKeyPairSync, sign } from 'node:crypto';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import * as api from 'verify-oxsum-bill';
import { inspect, scopedBill, scopedConsistency, scopedEvidence } from './scope.mjs';

const read = (name) => readFileSync(new URL('./fixtures/'+name+'.json', import.meta.url), 'utf8');
const evidence = read('evidence-bundle'), manifest = read('evidence-manifest'), key = read('evidence-operator-key');
const ok = { ok: true };

test('lossless field selection and mutation preserve numeric tokens and reject ambiguous input', () => {
  const raw = '{"n":9007199254740993,"escaped\\"key":[{"a":"x\\\\y"}],"flag":true}';
  const view = inspect(raw);
  assert.equal(view.token(['n']), '9007199254740993');
  assert.equal(view.get(['escaped"key', 0, 'a']), 'x\\y');
  assert.equal(inspect(view.replace(['flag'], 'false')).token(['n']), '9007199254740993');
  assert.throws(() => view.replace(['flag'], 'true'), /did not change/);
  assert.throws(() => view.replace(['missing'], '0'), /absent/);
  for (const bad of ['{"n":1,"n":2}', '{"a":{"n":1,"\\u006e":2}}', '[1,]']) assert.throws(() => inspect(bad));
});

test('scope passes original large integer bytes to each verifier without promising a synthetic signature', async () => {
  const raw = '{"bundleId":"bundle","organizationId":"customer","note":"signed","head":{"size":9007199254740993,"root":"root"},"amount":9223372036854775807}';
  const retained = '{"bundleId":"bundle","organizationId":"customer","origin":"retained"}';
  let calls = 0;
  await scopedEvidence(raw, retained, key, {
    async verifyEvidenceBundle(value, organization) { assert.equal(value, raw); assert.equal(organization, 'customer'); calls++; return ok; },
    async verifySignedHead(note, head, origin) { assert.equal(head, '{"size":9007199254740993,"root":"root"}'); assert.equal(origin, 'retained'); calls++; return ok; },
  });
  assert.equal(calls, 2);
});

test('packed WASM binds genuine period evidence to retained identity, origin and key', async () => {
  assert.equal((await scopedEvidence(evidence, manifest, key, api)).ok, true);
  for (const field of ['bundleId', 'organizationId', 'origin']) {
    const changed = inspect(manifest).replace([field], JSON.stringify('foreign'));
    await assert.rejects(scopedEvidence(evidence, changed, key, api));
  }
  await assert.rejects(scopedEvidence(evidence, manifest, inspect(key).replace(['publicKey'], JSON.stringify('00'.repeat(32))), api));
  await assert.rejects(scopedEvidence(inspect(evidence).replace(['note'], 'null'), manifest, key, api));
  await assert.rejects(scopedEvidence(evidence, '{}', key, api), /Missing required/);
});

test('packed WASM self-declared signed foreign origin cannot replace the retained origin', async () => {
  const { privateKey, publicKey } = generateKeyPairSync('ed25519');
  const bytes = publicKey.export({ format: 'der', type: 'spki' }).subarray(-32);
  const keyName = 'oxsum/foreign-test';
  const selector = createHash('sha256').update(Buffer.concat([Buffer.from(keyName+'\n'), Buffer.from([1]), bytes])).digest().subarray(0, 4);
  const view = inspect(evidence);
  const body = `oxsum/ledgers/foreign\n${view.token(['head','size'])}\n${Buffer.from(view.get(['head','root']), 'hex').toString('base64')}\n`;
  const foreignKey = { keyName, publicKey: bytes.toString('hex'), keyHash: selector.toString('hex') };
  const note = `${body}\n— ${keyName} ${Buffer.concat([selector, sign(null, Buffer.from(body), privateKey)]).toString('base64')}\n`;
  let foreign = view.replace(['note'], JSON.stringify(note));
  for (const [field, value] of Object.entries(foreignKey)) foreign = inspect(foreign).replace([field], JSON.stringify(value));
  const retainedKey = JSON.stringify(foreignKey);
  assert.equal((await api.verifyEvidenceBundle(foreign, inspect(manifest).get(['organizationId']), retainedKey)).ok, true);
  await assert.rejects(scopedEvidence(foreign, manifest, retainedKey, api), /different ledger/);
});

for (const folder of ['', 'sale/']) test(`packed WASM ${folder || 'legacy/'} bill enforces separate catalog verdicts and retained heads`, async () => {
  const raw = read(folder+'settlement-bundle'), retained = read(folder+'manifest'), head = read(folder+'signed-head'), operator = read(folder+'operator-key');
  const view = inspect(raw);
  assert.equal((await scopedBill(raw, retained, head, operator, api)).ok, true);
  for (const field of ['entryId', 'origin', 'headRoot']) await assert.rejects(scopedBill(raw, inspect(retained).replace([field], '"wrong"'), head, operator, api));
  const binding = folder ? 'sale' : 'price';
  for (const changed of [view.replace([binding], 'null'), view.replace([binding,'row'], JSON.stringify(view.get([binding,'row'])+' '))]) {
    // Baseline API intentionally keeps inclusion/arithmetic success separate.
    assert.equal((await api.verifyBill(changed, inspect(retained).get(['contentHash']))).ok, true);
    await assert.rejects(scopedBill(changed, retained, head, operator, api));
  }
  if (folder) assert.equal((await scopedBill(view.replace(['price'], 'null'), retained, head, operator, api)).ok, true);
});


test('packed WASM consistency must end at the bill head under review', async () => {
  const raw = read('consistency'), held = read('held-head'), head = read('signed-head'), operator = read('operator-key');
  const origin = inspect(read('manifest')).get(['origin']);
  assert.equal((await scopedConsistency(raw, held, head, origin, operator, api)).ok, true);
  await assert.rejects(scopedConsistency(raw, held, inspect(head).replace(['root'], JSON.stringify('00'.repeat(32))), origin, operator, api), /Retained head differs/);
});
