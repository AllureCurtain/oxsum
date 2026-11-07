import assert from "node:assert/strict";
import test from "node:test";
import { creditsText, modelDraft, modelsValue, termsValue, utc, utcMicros, scheduleState } from "../src/lib/sale-draft.ts";

test("sale rates round-trip exact micros, zero and optional conditional dimensions", () => {
  const term = { price: { inputPricePerMillion: 9007199254740991, outputPricePerMillion: 1, maxOutputTokens: 32,
    cacheReadPricePerMillion: 0, costPerRequest: 123, mode: "chat" as const,
    rules: [{ match: { serviceTier: "priority", minInputTokens: 1000 }, price: { inputPricePerMillion: 1, outputPricePerMillion: 0, maxOutputTokens: 64 } }] }, discountPercent: 15 };
  assert.deepEqual(termsValue(modelDraft("priced", term)), term);
  assert.equal(creditsText(1), "0.000001");
  const row = modelDraft("free");
  assert.equal(modelsValue([row]).free.price.inputPricePerMillion, 0);
  row.price.inputPricePerMillion = "0.0000001";
  assert.throws(() => termsValue(row), /six fractional/);
  row.price.inputPricePerMillion = "9007199254.740992";
  assert.throws(() => termsValue(row), /exact nonnegative/);
  assert.throws(() => modelsValue([modelDraft("same"), modelDraft("same")]), /unique/);
  row.price.inputPricePerMillion = "0"; row.mode = "event";
  assert.throws(() => termsValue(row), /Non-chat/);
  row.price.maxOutputTokens = "0"; assert.equal(termsValue(row).price.mode, "event");
});
test("schedules honor exact UTC boundaries and reject ambiguous local dates", () => {
  assert.equal(utc("2028-02-29T00:00:00Z"), "2028-02-29T00:00:00.000Z");
  for (const text of ["2026-02-29T00:00:00Z", "2026-12-01T00:00", "2026-12-01T00:00:00+08:00"]) assert.throws(() => utc(text));
  assert.equal(utc("2026-12-01T00:00:00.123456Z"), "2026-12-01T00:00:00.123456Z");
  const row = { effectiveFrom: "2026-12-01T00:00:00Z", effectiveUntil: "2026-12-02T00:00:00Z" };
  assert.equal(scheduleState(row, Date.parse(row.effectiveFrom)), "Active");
  assert.equal(scheduleState(row, Date.parse(row.effectiveUntil)), "Ended");
  assert.equal(scheduleState({ ...row, effectiveUntil: row.effectiveFrom }, 0), "Canceled");
  assert.equal(utcMicros("2026-12-01T00:00:00.000001Z") - utcMicros("2026-12-01T00:00:00Z"), 1n);
  assert.equal(scheduleState({ ...row, effectiveUntil: "2026-12-01T00:00:00.000001Z" }, Date.parse(row.effectiveFrom)), "Active");
});
