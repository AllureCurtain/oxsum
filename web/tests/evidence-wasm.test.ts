/** Requires scripts/build-verify-wasm.sh or an existing OXSUM_VERIFY_WASM_DIR. */
import assert from "node:assert/strict";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";
import { expectedEvidence, verifyEvidence, type EvidenceBundle, type EvidenceVerifier } from "../src/lib/evidence.ts";
import { retainHead, type SignedHead } from "../src/lib/retained-head.ts";

const folder = process.env.OXSUM_VERIFY_WASM_DIR ? pathToFileURL(resolve(process.env.OXSUM_VERIFY_WASM_DIR) + "/") : new URL("../../crates/web/verify-wasm/", import.meta.url);
const wasm = await import(new URL("verify_wasm.js", folder).href) as EvidenceVerifier & { default: (options: unknown) => Promise<unknown> };
await wasm.default({ module_or_path: await readFile(new URL("verify_wasm_bg.wasm", folder)) });
const bundle: EvidenceBundle = JSON.parse(await readFile(new URL("../../examples/verify-consumer/fixtures/evidence-bundle.json", import.meta.url), "utf8"));
const origin = bundle.note!.split("\n")[0];
const identity = { site: "https://billing.example", organizationId: bundle.organizationId, tenantId: origin.slice("oxsum/ledgers/".length) };
const expected = expectedEvidence(identity, bundle);
const retained = await retainHead(identity, { ...bundle.head, note: bundle.note, origin, keyName: bundle.keyName, publicKey: bundle.publicKey, keyHash: bundle.keyHash } as SignedHead, wasm);

test("actual WASM verifies frozen period evidence and distinguishes retained/supplied/unsigned keys", async () => {
  assert.equal((await verifyEvidence(JSON.stringify(bundle), expected, identity, retained, wasm)).signature, "retainedKey");
  assert.equal((await verifyEvidence(JSON.stringify(bundle), expected, identity, null, wasm)).signature, "suppliedKey");
  assert.equal((await verifyEvidence(JSON.stringify({ ...bundle, note: null }), expected, identity, retained, wasm)).signature, "unsigned");
});

test("actual WASM rejects modified amounts, missing artifacts, wrong organization, manifest and key", async () => {
  const amount = structuredClone(bundle);
  const artifacts = amount.artifacts as Record<string, Record<string, Record<string, unknown>>>;
  const usage = Object.values(artifacts.usageRecord)[0]; usage.chargedMinor = Number(usage.chargedMinor) + 1;
  const missing = structuredClone(bundle);
  const rows = (missing.artifacts as Record<string, Record<string, unknown>>).usageRecord;
  delete rows[Object.keys(rows)[0]];
  for (const changed of [amount, missing, { ...bundle, organizationId: "other" }, { ...bundle, manifestDigest: "ab".repeat(32) }, { ...bundle, publicKey: "cd".repeat(32) }]) await assert.rejects(verifyEvidence(JSON.stringify(changed), expected, identity, retained, wasm));
});

test("a genuine signed foreign origin is rejected even though the evidence wrapper accepts its self-declared origin", async () => {
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  const bytes = publicKey.export({ format: "der", type: "spki" }).subarray(-32);
  const keyName = "oxsum/foreign-test";
  const selector = createHash("sha256").update(Buffer.concat([Buffer.from(keyName + "\n"), Buffer.from([1]), bytes])).digest().subarray(0, 4);
  const body = `oxsum/ledgers/foreign\n${bundle.head.size}\n${Buffer.from(bundle.head.root!, "hex").toString("base64")}\n`;
  const key = { keyName, publicKey: bytes.toString("hex"), keyHash: selector.toString("hex") };
  const note = `${body}\n— ${keyName} ${Buffer.concat([selector, sign(null, Buffer.from(body), privateKey)]).toString("base64")}\n`;
  const foreign = { ...bundle, ...key, note };
  assert.equal(JSON.parse(await wasm.verifyEvidenceBundle(JSON.stringify(foreign), identity.organizationId, JSON.stringify(key))).ok, true);
  await assert.rejects(verifyEvidence(JSON.stringify(foreign), expected, identity, null, wasm), /different ledger/);
});
