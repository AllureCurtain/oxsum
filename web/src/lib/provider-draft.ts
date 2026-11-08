import type { components } from "../api/schema";
import { decimalMinor } from "./money.ts";

type Schemas = components["schemas"];
export type ModelPrice = Schemas["ModelPrice"];
export type ProviderPrice = Schemas["BusinessProviderPriceRequest"]["config"];
export const RATE_KEYS = ["inputPricePerMillion", "outputPricePerMillion", "cacheReadPricePerMillion",
  "cacheWrite5mPricePerMillion", "cacheWrite1hPricePerMillion", "reasoningPricePerMillion", "costPerRequest"] as const;
export type RateKey = (typeof RATE_KEYS)[number];
export type Rates = Record<RateKey, string>;
export type PriceDraft = { rates: Rates; maxOutputTokens: string; upstream: Rates | null };
export type RuleDraft = { serviceTier: string; minInputTokens: string; maxInputTokens: string; price: PriceDraft };
export type ProviderDraft = { model: string; mode: NonNullable<ModelPrice["mode"]>; weight: string; price: PriceDraft; rules: RuleDraft[] };

function fail(code: string): never { throw new Error(code); }
export function whole(raw: string, minimum = 0, maximum = Number.MAX_SAFE_INTEGER): number {
  const text = raw.trim();
  if (!/^\d+$/.test(text) || text.length > 16 || BigInt(text) < BigInt(minimum) || BigInt(text) > BigInt(maximum)) fail("integer");
  return Number(text);
}
function creditText(value: number | undefined): string {
  if (value === undefined) return "";
  if (!Number.isSafeInteger(value) || value < 0) fail("storedRange");
  const n = BigInt(value);
  return `${n / 1000000n}.${String(n % 1000000n).padStart(6, "0")}`;
}
export function rateDraft(value?: Schemas["UpstreamPrices"]): Rates {
  return Object.fromEntries(RATE_KEYS.map((key) => [key, creditText(value?.[key])])) as Rates;
}
function priceDraft(value?: Schemas["PriceSet"]): PriceDraft {
  const max = value?.maxOutputTokens ?? 1024;
  if (!Number.isSafeInteger(max) || max < 0) fail("storedRange");
  return { rates: rateDraft(value ?? { inputPricePerMillion: 0, outputPricePerMillion: 0 }), maxOutputTokens: String(max),
    upstream: value?.upstream === undefined ? null : rateDraft(value.upstream) };
}
export function providerDraft(value?: ModelPrice): ProviderDraft {
  const weight = value?.weight ?? 100;
  if (!Number.isSafeInteger(weight) || weight < 1 || weight > 1000) fail("storedRange");
  return { model: value?.model ?? "", mode: value?.mode ?? "chat", weight: String(weight), price: priceDraft(value),
    rules: (value?.rules ?? []).map((rule) => {
      for (const bound of [rule.match.minInputTokens, rule.match.maxInputTokens]) {
        if (bound !== undefined && (!Number.isSafeInteger(bound) || bound < 0)) fail("storedRange");
      }
      return { serviceTier: rule.match.serviceTier ?? "", minInputTokens: String(rule.match.minInputTokens ?? ""),
        maxInputTokens: String(rule.match.maxInputTokens ?? ""), price: priceDraft(rule.price) };
    }) };
}
function rateValue(draft: Rates, required = false): Schemas["UpstreamPrices"] {
  const result: Schemas["UpstreamPrices"] = {};
  for (const key of RATE_KEYS) {
    if (!draft[key].trim()) {
      if (required && (key === "inputPricePerMillion" || key === "outputPricePerMillion")) fail("rate");
      continue;
    }
    const value = decimalMinor(draft[key], 6, true);
    if (value === null) fail("rate");
    // The base per-request rate is multiplied by one million in the Rust rater.
    if (required && key === "costPerRequest" && BigInt(value) > 9223372036854n) fail("flatRate");
    result[key] = value;
  }
  return result;
}
function priceValue(draft: PriceDraft, mode: ProviderDraft["mode"]): Schemas["PriceSet"] & { outputPricePerMillion: number; maxOutputTokens: number } {
  const rates = rateValue(draft.rates, true);
  const maximum = whole(draft.maxOutputTokens);
  if ((mode === "chat" && maximum === 0) || (mode !== "chat" && maximum !== 0)
    || ((mode === "embeddings" || mode === "rerank") && rates.outputPricePerMillion !== 0)) fail("mode");
  return { ...rates, inputPricePerMillion: rates.inputPricePerMillion!, outputPricePerMillion: rates.outputPricePerMillion!,
    maxOutputTokens: maximum, ...(draft.upstream === null ? {} : { upstream: rateValue(draft.upstream) }) };
}
export function modelName(raw: string): string {
  const value = raw.trim();
  if (!value || value.length > 200 || /[\u0000-\u001f\u007f]/.test(value)) fail("name");
  return value;
}
export function providerValue(draft: ProviderDraft): ProviderPrice {
  if (draft.rules.length > 100) fail("rules");
  const rules = draft.rules.map((rule) => {
    const match: Schemas["PriceMatch"] = {};
    if (rule.serviceTier.length) {
      if (rule.serviceTier.length > 64) fail("rules");
      // Tier names match exact upstream text; whitespace is meaningful history.
      match.serviceTier = rule.serviceTier;
    }
    if (rule.minInputTokens.trim()) match.minInputTokens = whole(rule.minInputTokens);
    if (rule.maxInputTokens.trim()) match.maxInputTokens = whole(rule.maxInputTokens);
    if (!Object.keys(match).length || (match.minInputTokens !== undefined && match.maxInputTokens !== undefined
      && match.minInputTokens > match.maxInputTokens)) fail("rules");
    return { match, price: priceValue(rule.price, draft.mode) };
  });
  return { model: modelName(draft.model), weight: whole(draft.weight, 1, 1000),
    price: { ...priceValue(draft.price, draft.mode), mode: draft.mode, rules } };
}
export function fallbackValue(primary: string, text: string): string[] {
  const names = text.split(/\r?\n/).map((name) => name.trim()).filter(Boolean).map(modelName);
  if (names.length > 16 || new Set(names).size !== names.length || names.includes(primary)) fail("fallbacks");
  return names;
}
export function cacheValue(state: "default" | "disabled" | "enabled", expiry: "bounded" | "forever", ttl: string): Schemas["BusinessProviderCacheRequest"]["config"] {
  return { policy: state === "default" ? null : { enabled: state === "enabled", ttlSeconds: expiry === "forever" ? null : whole(ttl, 1) } };
}
