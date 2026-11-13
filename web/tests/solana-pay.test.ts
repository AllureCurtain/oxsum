import assert from "node:assert/strict";
import test from "node:test";
import { baseUnitsDecimal, solanaPayUrl } from "../src/lib/solanaPay.ts";

// The intent's fixed terms for the URL cases: a devnet deposit address, a
// per-intent marker, and the mint a configured SPL rail would name.
const PAY_TO = "depoS1t7sVbqZ9zY3vQ2hJq9b5qY8hA1v5oPq4n6m7k8j";
const REFERENCE = "ref1111111111111111111111111111111111111111111";
const MINT = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

test("a native SOL intent emits amount at 9 decimals and no spl-token", () => {
  const url = solanaPayUrl({
    recipient: PAY_TO,
    baseUnits: 5_000,
    assetDecimals: 9,
    reference: REFERENCE,
    label: "oxsum top-up",
    memo: "dep-01",
  });
  assert.equal(
    url,
    `solana:${PAY_TO}?amount=0.000005&reference=${REFERENCE}&label=oxsum+top-up&memo=dep-01`,
  );
});

test("an SPL intent names the mint and converts at its 6 decimals", () => {
  const url = solanaPayUrl({
    recipient: PAY_TO,
    baseUnits: 1_234_500,
    assetDecimals: 6,
    splToken: MINT,
    reference: REFERENCE,
  });
  assert.equal(
    url,
    `solana:${PAY_TO}?amount=1.2345&spl-token=${MINT}&reference=${REFERENCE}`,
  );
});

test("baseUnitsDecimal writes exact decimals and never floats", () => {
  assert.equal(baseUnitsDecimal(5_000, 9), "0.000005");
  assert.equal(baseUnitsDecimal(1_000_000_000, 9), "1");
  assert.equal(baseUnitsDecimal(1_234_500, 6), "1.2345");
  assert.equal(baseUnitsDecimal(1, 0), "1");
  assert.equal(baseUnitsDecimal(0, 9), "0");
  // Whole-asset amounts keep no trailing zeros; a fraction keeps its own.
  assert.equal(baseUnitsDecimal(2_500_000_000, 9), "2.5");
  assert.equal(baseUnitsDecimal(100_000_000, 9), "0.1");
});

test("baseUnitsDecimal refuses a figure it cannot write honestly", () => {
  assert.equal(baseUnitsDecimal(-1, 9), null);
  assert.equal(baseUnitsDecimal(0.5, 9), null);
  assert.equal(baseUnitsDecimal(1, -1), null);
  assert.equal(baseUnitsDecimal(1, 31), null);
});

test("label and memo are encoded so their text cannot inject parameters", () => {
  const url = solanaPayUrl({
    recipient: PAY_TO,
    baseUnits: 1,
    assetDecimals: 9,
    reference: REFERENCE,
    label: "top-up & pay",
    memo: "amount=999&reference=evil",
  });
  assert.ok(url !== null);
  assert.ok(
    url.includes("memo=amount%3D999%26reference%3Devil"),
    url,
  );
  // Exactly one reference and one amount parameter survive — the memo's text
  // stayed memo text.
  assert.equal(url.split("reference=").length - 1, 1);
  assert.equal(url.split("amount=").length - 1, 1);
});

test("a request with no usable recipient, reference or amount answers null", () => {
  const base = { recipient: PAY_TO, baseUnits: 1, assetDecimals: 9, reference: REFERENCE };
  assert.equal(solanaPayUrl({ ...base, recipient: " " }), null);
  assert.equal(solanaPayUrl({ ...base, reference: "" }), null);
  assert.equal(solanaPayUrl({ ...base, baseUnits: -5 }), null);
});

test("empty label and memo stay off the URL entirely", () => {
  const url = solanaPayUrl({
    recipient: PAY_TO,
    baseUnits: 42,
    assetDecimals: 0,
    reference: REFERENCE,
    label: "",
    memo: "",
  });
  assert.equal(url, `solana:${PAY_TO}?amount=42&reference=${REFERENCE}`);
});
