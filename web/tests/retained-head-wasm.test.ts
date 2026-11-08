/** Run after scripts/build-verify-wasm.sh, or set OXSUM_VERIFY_WASM_DIR to an existing build.
 * Missing/placeholder WASM fails the suite rather than skipping the cryptographic checks. */
import assert from "node:assert/strict";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";
import { checkHead, retainHead, type HeadVerifier, type TreeHead } from "../src/lib/retained-head.ts";

const folder = process.env.OXSUM_VERIFY_WASM_DIR ? pathToFileURL(resolve(process.env.OXSUM_VERIFY_WASM_DIR) + "/") : new URL("../../crates/web/verify-wasm/", import.meta.url);
const wasm = await import(new URL("verify_wasm.js", folder).href) as HeadVerifier & { default: (options: unknown) => Promise<unknown> };
await wasm.default({ module_or_path: await readFile(new URL("verify_wasm_bg.wasm", folder)) });
const digest = (...bytes: Buffer[]) => createHash("sha256").update(Buffer.concat(bytes)).digest();
const identity = { site: "https://billing.example", organizationId: "org-a", tenantId: "customer-a" };
const origin = `oxsum/ledgers/${identity.tenantId}`;
const { privateKey, publicKey } = generateKeyPairSync("ed25519");
const publicBytes = publicKey.export({ type: "spki", format: "der" }).subarray(-32);
const keyName = "oxsum/test";
const selector = digest(Buffer.from(keyName + "\n"), Buffer.from([1]), publicBytes).subarray(0, 4);
const key = { keyName, publicKey: publicBytes.toString("base64"), keyHash: selector.toString("hex") };
function signed(head: TreeHead, ledger = origin) {
  const body = `${ledger}\n${head.size}\n${Buffer.from(head.root, "hex").toString("base64")}\n`;
  const signature = Buffer.concat([selector, sign(null, Buffer.from(body), privateKey)]).toString("base64");
  return { ...head, ...key, origin: ledger, note: `${body}\n— ${keyName} ${signature}\n` };
}
// Reuse actual engine-produced BLAKE3 Merkle fixtures. Signing new notes with
// a generated test key allows independent signature and history substitutions.
const fixture = JSON.parse(await readFile(new URL("../../examples/verify-consumer/fixtures/consistency.json", import.meta.url), "utf8"));
const old = signed(fixture.oldHead);
const next = signed(fixture.head);
const held = await retainHead(identity, old, wasm);
const response = { ...key, note: next.note, oldHead: held.head, head: fixture.head, proof: fixture.proof };
const noFetch = async () => { throw new Error("unexpected request"); };

test("actual WASM verifies retained signatures and consistency without advancing the archive", async () => {
  const initial = JSON.stringify(held);
  assert.equal((await checkHead(held, next, wasm, async () => response)).kind, "extended");
  assert.equal((await checkHead(held, old, wasm, noFetch)).kind, "unchanged");
  assert.equal(JSON.stringify(held), initial);
});

test("actual WASM rejects a valid HTTP response with altered old root, proof path or signature", async () => {
  for (const changed of [
    { ...response, oldHead: { ...response.oldHead, root: "ab".repeat(32) } },
    { ...response, proof: { ...response.proof, path: ["cd".repeat(32)] } },
    { ...response, note: response.note.replace("— ", "— other/") },
  ]) await assert.rejects(checkHead(held, next, wasm, async () => changed));
  await assert.rejects(retainHead(identity, { ...old, note: old.note.replace("customer-a", "customer-b") }, wasm));
  await assert.rejects(checkHead({ ...held, head: { ...held.head, root: "ef".repeat(32) } }, next, wasm, async () => response));
  await assert.rejects(retainHead(identity, { ...old, publicKey: "ab".repeat(32) }, wasm));
});

test("actual signed empty, rollback, same-size fork and key/origin changes remain distinct", async () => {
  const empty = signed({ size: 0, root: digest(Buffer.alloc(0)).toString("hex") });
  const zeroHeld = await retainHead(identity, empty, wasm);
  assert.equal((await checkHead(zeroHeld, next, wasm, noFetch)).kind, "emptyAnchor");
  await assert.rejects(checkHead(held, empty, wasm, noFetch), /smaller/);
  await assert.rejects(checkHead(held, signed({ size: 1, root: "cd".repeat(32) }), wasm, noFetch), /different root/);
  await assert.rejects(checkHead(held, signed(next, "oxsum/ledgers/other"), wasm, noFetch), /origin or operator key changed/);
  await assert.rejects(checkHead(held, { ...next, keyHash: "deadbeef" }, wasm, noFetch), /key changed/);
});


test("a later valid consistency response cannot hide a contradictory intermediate observed head", async () => {
  const intermediate = signed({ size: 2, root: "ab".repeat(32) });
  // The response is a valid 1→3 extension, but says nothing about this observed 2.
  await assert.rejects(checkHead(held, intermediate, wasm, async () => response), /differs from the observed head/);
});
