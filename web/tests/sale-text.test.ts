import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import i18next from "i18next";
import { modelDraft, termsValue, utc } from "../src/lib/sale-draft.ts";
import { saleError, saleLabel } from "../src/lib/sale-text.ts";

const en = JSON.parse(readFileSync(new URL("../src/locales/en.json", import.meta.url), "utf8"));
const zh = JSON.parse(readFileSync(new URL("../src/locales/zh-CN.json", import.meta.url), "utf8"));
const i18n = i18next.createInstance();
await i18n.init({ resources: { en: { translation: en }, "zh-CN": { translation: zh } }, lng: "en", interpolation: { escapeValue: false } });

function captured(action: () => unknown): unknown {
  try { action(); assert.fail("Expected rejected exact input"); } catch (error) { return error; }
}

test("sale validation renders exact failures in either locale without changing parsing", () => {
  const row = modelDraft("browser-chat");
  row.price.inputPricePerMillion = "0.0000001";
  const error = captured(() => termsValue(row));
  const english = i18n.getFixedT("en"), chinese = i18n.getFixedT("zh-CN");
  assert.match(saleError(error, english, "fallback"), /Input credits per million.*six fractional/);
  assert.equal(saleError(error, chinese, "fallback"), "每百万输入 token 的积分：请使用精确的非负金额，最多六位小数");
  assert.equal(saleError(captured(() => utc("2026-02-29T00:00:00Z")), chinese, "fallback"), "UTC 日历日期无效");
  row.price.inputPricePerMillion = "0.000001";
  assert.equal(termsValue(row).price.inputPricePerMillion, 1);
  assert.equal(saleLabel("override", chinese), "客户专属");
  assert.equal(saleLabel("chat", chinese), "对话");
  assert.equal(saleError(new Error("overlapping sale windows in the same scope"), chinese, "fallback"), "同一范围内的销售生效区间重叠");
  assert.equal(saleError(new Error("upstream diagnostic 47"), chinese, "fallback"), "upstream diagnostic 47");
});

test("sale locale interpolation preserves all dynamic values", () => {
  assert.deepEqual(Object.keys(en.sale).sort(), Object.keys(zh.sale).sort());
  for (const [key, value] of Object.entries(en.sale)) {
    const placeholders = (text: string) => [...text.matchAll(/{{(.*?)}}/g)].map((match) => match[1]).sort();
    assert.deepEqual(placeholders(zh.sale[key]), placeholders(String(value)), key);
    assert.ok(zh.sale[key].trim());
  }
  const t = i18n.getFixedT("zh-CN");
  assert.equal(t("sale.savedDraft", { revision: 2 }), "已保存修订 2。发布指定修订并安排生效后才会启用。");
  assert.match(t("sale.resolution", { source: "客户专属", book: "book-1", version: 2, revision: 3, schedule: "schedule-4" }), /book-1.*2.*3.*schedule-4/);
});
