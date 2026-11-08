import assert from "node:assert/strict";
import test from "node:test";
import { evidenceBody, evidencePeriod, expectedEvidence, matchesEvidence, verifyEvidence, type EvidenceBundle, type EvidenceVerifier } from "../src/lib/evidence.ts";
const now = Date.UTC(2026, 10, 9, 23);

test("evidence spans use valid inclusive ended UTC days with a 366-day maximum", () => {
  assert.deepEqual(evidencePeriod("2026-11-01", "2026-11-08", now), { from: "2026-11-01", to: "2026-11-08" });
  assert.deepEqual(evidencePeriod("2024-01-01", "2024-12-31", now), { from: "2024-01-01", to: "2024-12-31" });
  for (const [from, to] of [["2026-02-30", "2026-03-01"], ["2026-11-01", "2026-11-09"], ["2026-11-08", "2026-11-01"], ["2024-01-01", "2025-01-01"], ["", "2026-01-01"]]) assert.throws(() => evidencePeriod(from, to, now));
});

const identity = { site: "https://billing.example", organizationId: "customer-a", tenantId: "tenant-a" };
const bundle: EvidenceBundle = { version: 2, kind: "period", bundleId: "bundle-a", organizationId: identity.organizationId, manifestDigest: "ab".repeat(32), anchorLogIndex: 1, scope: { covered: [], excluded: [], upstreamCostIncluded: false }, artifacts: {}, manifest: [], entries: [], head: { size: 1, root: "cd".repeat(32) } };
const expected = expectedEvidence(identity, bundle);
const pass: EvidenceVerifier = { verifySignedHead: () => '{"ok":true}', verifyConsistency: () => '{"ok":true}', verifyEvidenceBundle: () => '{"ok":true}' };

test("customer and separately selected manifest identity cannot be replaced by bundle fields", async () => {
  for (const change of [{ organizationId: "other" }, { bundleId: "other" }, { manifestDigest: "ef".repeat(32) }, { version: 1 }]) await assert.rejects(verifyEvidence(JSON.stringify({ ...bundle, ...change }), expected, identity, null, pass), /differs/);
  await assert.rejects(verifyEvidence(JSON.stringify(bundle), { ...expected, origin: "oxsum/ledgers/other" }, identity, null, pass), /differs/);
});

test("a local positive evidence verdict is required, and unsigned evidence stays unsigned", async () => {
  for (const value of ['{"ok":false}', '{"ok":"true"}', '{}', 'null', 'invalid']) await assert.rejects(verifyEvidence(JSON.stringify(bundle), expected, identity, null, { ...pass, verifyEvidenceBundle: () => value }));
  assert.equal((await verifyEvidence(JSON.stringify(bundle), expected, identity, null, pass)).signature, "unsigned");
  await assert.rejects(verifyEvidence(JSON.stringify({ ...bundle, note: "" }), expected, identity, null, pass), /malformed/);
  await assert.rejects(verifyEvidence(JSON.stringify({ ...bundle, note: "signed" }), expected, identity, null, pass), /incomplete/);
});


test("raw evidence preserves exact large integers and only accepts the contract data envelope", async () => {
  const json = JSON.stringify(bundle).replace('"artifacts":{}', '"artifacts":{"amountMinor":9007199254740993}');
  const parsed = evidenceBody('{"data":' + json + '}');
  assert.equal(parsed.json, json);
  await verifyEvidence(parsed.json, expected, identity, null, { ...pass, verifyEvidenceBundle: (bytes) => { assert.equal(bytes, json); return '{"ok":true}'; } });
  for (const value of ['{"data":null}', '{"data":{},"other":1}', '{"data":{},"data":{}}', '{"data":[]}', '{}', 'invalid']) assert.throws(() => evidenceBody(value));
});


test("selected period and statement metadata must match even when the digest is unchanged", () => {
  const summary = { ...bundle, periodFrom: "2026-11-01", periodTo: "2026-11-08", entryCount: 1, createdAt: "2026-11-09T00:00:00Z" };
  assert.equal(matchesEvidence(summary, summary), true);
  for (const change of [{ kind: "statement" }, { periodFrom: "2026-10-01" }, { periodTo: "2026-11-07" }, { statementId: "other-statement" }]) assert.equal(matchesEvidence(summary, { ...summary, ...change } as EvidenceBundle), false);
});
