import assert from "node:assert/strict";
import test from "node:test";
import { cacheValue, fallbackValue, providerDraft, providerValue, type ModelPrice } from "../src/lib/provider-draft.ts";

test("model edits preserve exact base, conditional, supplier costs and routing weight", () => {
  const version: ModelPrice = { model: "model/a", version: 4, createdAt: "2026-11-08T00:00:00Z", weight: 17, mode: "chat",
    inputPricePerMillion: 9007199254740991, outputPricePerMillion: 1, maxOutputTokens: 64, cacheReadPricePerMillion: 0,
    cacheWrite5mPricePerMillion: 2, cacheWrite1hPricePerMillion: 3, reasoningPricePerMillion: 4, costPerRequest: 5,
    upstream: { inputPricePerMillion: 7, cacheReadPricePerMillion: 0, outputPricePerMillion: 9, costPerRequest: 13 },
    rules: [{ match: { serviceTier: " priority ", minInputTokens: 0, maxInputTokens: 123 }, price: {
      inputPricePerMillion: 10, outputPricePerMillion: 11, maxOutputTokens: 32, reasoningPricePerMillion: 0,
      upstream: { inputPricePerMillion: 12, reasoningPricePerMillion: 14 } } }] };
  const { model, weight, version: _version, createdAt: _created, ...price } = version;
  assert.deepEqual(providerValue(providerDraft(version)), { model, weight, price });
  const weighted = providerDraft(version); weighted.weight = "19";
  assert.equal(providerValue(weighted).price.rules?.[0].match.serviceTier, " priority ");
  const draft = providerDraft(version);
  assert.equal(draft.price.rates.inputPricePerMillion, "9007199254.740991");
  assert.equal(draft.price.upstream?.cacheReadPricePerMillion, "0.000000");
  assert.equal(draft.price.upstream?.cacheWrite5mPricePerMillion, "");
  draft.price.upstream = null;
  assert.equal(providerValue(draft).price.upstream, undefined);
  assert.deepEqual(providerValue(providerDraft({ ...version, upstream: {} })).price.upstream, {});
});

test("unsafe stored or edited amounts refuse instead of rounding or defaulting", () => {
  const version: ModelPrice = { model: "fixture", version: 1, createdAt: "", weight: 100,
    inputPricePerMillion: 9007199254740992, outputPricePerMillion: 0, maxOutputTokens: 32 };
  assert.throws(() => providerDraft(version), /storedRange/);
  const draft = providerDraft(); draft.model = "fixture";
  for (const invalid of ["0.0000001", "9007199254.740992", "-1", "Infinity", "1e3", ""]) {
    draft.price.rates.inputPricePerMillion = invalid;
    assert.throws(() => providerValue(draft), /rate/);
  }
  draft.price.rates.inputPricePerMillion = "0";
  draft.price.rates.costPerRequest = "9223372.036855";
  assert.throws(() => providerValue(draft), /flatRate/);
  draft.price.rates.costPerRequest = "9223372.036854";
  assert.equal(providerValue(draft).price.costPerRequest, 9223372036854);
  for (const weight of ["0", "1001", "1.5", "-1"]) {
    draft.weight = weight; assert.throws(() => providerValue(draft), /integer/);
  }
});

test("changing mode requires explicit review of every existing output field", () => {
  const draft = providerDraft(); draft.model = "fixture";
  draft.mode = "embeddings";
  assert.throws(() => providerValue(draft), /mode/);
  draft.price.maxOutputTokens = "0";
  draft.price.rates.outputPricePerMillion = "1";
  assert.throws(() => providerValue(draft), /mode/);
  draft.price.rates.outputPricePerMillion = "0";
  draft.rules = [{ serviceTier: "priority", minInputTokens: "", maxInputTokens: "", price: structuredClone(draft.price) }];
  draft.rules[0].price.maxOutputTokens = "1";
  assert.throws(() => providerValue(draft), /mode/);
  draft.rules[0].price.maxOutputTokens = "0";
  assert.equal(providerValue(draft).price.mode, "embeddings");
  draft.rules[0].minInputTokens = "10"; draft.rules[0].maxInputTokens = "9";
  assert.throws(() => providerValue(draft), /rules/);
});

test("fallback order and cache expiry are explicit and bounded", () => {
  assert.deepEqual(fallbackValue("primary", " model/b\n\nmodel/a \n"), ["model/b", "model/a"]);
  assert.deepEqual(fallbackValue("primary", ""), []);
  for (const text of ["primary", "same\nsame", Array.from({ length: 17 }, (_, i) => `m${i}`).join("\n")]) {
    assert.throws(() => fallbackValue("primary", text), /fallbacks/);
  }
  assert.deepEqual(cacheValue("default", "bounded", ""), { policy: null });
  assert.deepEqual(cacheValue("disabled", "bounded", "300"), { policy: { enabled: false, ttlSeconds: 300 } });
  assert.deepEqual(cacheValue("enabled", "forever", ""), { policy: { enabled: true, ttlSeconds: null } });
  for (const ttl of ["", "0", "-1", "1.5", "9007199254740992"]) {
    assert.throws(() => cacheValue("enabled", "bounded", ttl), /integer/);
  }
});
