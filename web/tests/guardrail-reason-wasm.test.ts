/** Uses the real wasm-bindgen module and a proof captured from guardrails.rs.
 * Regenerate the fixture by setting OXSUM_GUARDRAIL_FIXTURE_OUT when running
 * eight_long_two_phase_reasons_survive_whole_cache_and_exact_replay. */
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";

const folder = process.env.OXSUM_VERIFY_WASM_DIR ? pathToFileURL(resolve(process.env.OXSUM_VERIFY_WASM_DIR) + "/") : new URL("../../crates/web/verify-wasm/", import.meta.url);
const wasm = await import(new URL("verify_wasm.js", folder).href);
await wasm.default({ module_or_path: await readFile(new URL("verify_wasm_bg.wasm", folder)) });
const fixture = JSON.parse(await readFile(new URL("fixtures/guardrail-reason.json", import.meta.url), "utf8"));
const description = JSON.parse(fixture.bundle.entry.description);

test("actual WASM verifies v15 bill inclusion and sixteen complete reason commitments", async () => {
  assert.equal(JSON.parse(await wasm.verifyBundle(JSON.stringify(fixture.bundle), fixture.expectedHash)).ok, true);
  assert.equal(description.v, 15);
  assert.equal(description.guardrail.length, 16);
  for (const note of description.guardrail) {
    assert.equal(Object.hasOwn(note, "reason"), false);
    assert.equal(note.reasonCommitment.sha256, createHash("sha256").update(fixture.reason, "utf8").digest("hex"));
    assert.equal(note.reasonCommitment.utf8Bytes, Buffer.byteLength(fixture.reason, "utf8"));
    assert.equal(JSON.parse(await wasm.verifyReasonCommitment(JSON.stringify(note.reasonCommitment), fixture.reason)).ok, true);
    assert.equal(JSON.parse(await wasm.verifyReasonCommitment(JSON.stringify(note.reasonCommitment), fixture.reason + " ")).ok, false);
  }
});

test("actual WASM rejects modified commitment bytes and exact-length substitutions", async () => {
  const altered = structuredClone(fixture.bundle);
  const changed = structuredClone(description);
  changed.guardrail[0].reasonCommitment.sha256 = "00".repeat(32);
  altered.entry.description = JSON.stringify(changed);
  assert.equal(JSON.parse(await wasm.verifyBundle(JSON.stringify(altered), fixture.expectedHash)).ok, false);
  const commitment = structuredClone(description.guardrail[0].reasonCommitment);
  commitment.utf8Bytes += 1;
  assert.equal(JSON.parse(await wasm.verifyReasonCommitment(JSON.stringify(commitment), fixture.reason)).ok, false);
  assert.equal(JSON.parse(await wasm.verifyReasonCommitment(JSON.stringify(description.guardrail[0].reasonCommitment), fixture.reason.replace("🦀", "🐢"))).ok, false);
});

test("actual WASM keeps invalid commitment shape separate from text mismatch", async () => {
  const original = description.guardrail[0].reasonCommitment;
  for (const raw of ["{}", JSON.stringify({ ...original, extra: true }), JSON.stringify({ ...original, utf8Bytes: 1.5 })]) {
    assert.throws(() => wasm.verifyReasonCommitment(raw, fixture.reason));
  }
  assert.equal(JSON.parse(await wasm.verifyReasonCommitment(JSON.stringify({ ...original, utf8Bytes: 0 }), fixture.reason)).ok, false);
});
