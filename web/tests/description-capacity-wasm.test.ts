/** Requires the actual scripts/build-verify-wasm.sh output; placeholders fail. */
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";

const folder = process.env.OXSUM_VERIFY_WASM_DIR ? pathToFileURL(resolve(process.env.OXSUM_VERIFY_WASM_DIR) + "/") : new URL("../../crates/web/verify-wasm/", import.meta.url);
const wasm = await import(new URL("verify_wasm.js", folder).href);
await wasm.default({ module_or_path: await readFile(new URL("verify_wasm_bg.wasm", folder)) });

for (const [label, count] of [["composed", 3114], ["max", 16384]] as const) {
  test(`actual WASM verifies ${label} evidence, detects tampering and refuses excess length`, async () => {
    const fixture = JSON.parse(await readFile(new URL(`../../crates/verify/fixtures/description-${label}.json`, import.meta.url), "utf8"));
    const description = fixture.bundle.entry.description;
    assert.equal([...description].length, count);
    const verdict = JSON.parse(wasm.verifyBundle(JSON.stringify(fixture.bundle), fixture.contentHash));
    assert.equal(verdict.ok, true);
    if (label === "composed") assert.match(verdict.charge, /recomputes/);
    const changed = structuredClone(fixture.bundle);
    changed.entry.description = "x" + [...description].slice(1).join("");
    assert.equal(JSON.parse(wasm.verifyBundle(JSON.stringify(changed), fixture.contentHash)).ok, false);
    changed.entry.description = "🧾".repeat(16385);
    assert.throws(() => wasm.verifyBundle(JSON.stringify(changed), fixture.contentHash));
    changed.entry.description = "bad\u0007";
    assert.throws(() => wasm.verifyBundle(JSON.stringify(changed), fixture.contentHash));
  });
}

test("actual WASM keeps existing short proof bytes valid", async () => {
  const bundle = JSON.parse(await readFile(new URL("../../examples/verify-consumer/fixtures/settlement-bundle.json", import.meta.url), "utf8"));
  const manifest = JSON.parse(await readFile(new URL("../../examples/verify-consumer/fixtures/manifest.json", import.meta.url), "utf8"));
  assert.equal(JSON.parse(wasm.verifyBundle(JSON.stringify(bundle), manifest.contentHash)).ok, true);
});
