import { decimalMinor } from "./money.ts";
import type { components } from "../api/schema";

type Price = components["schemas"]["SalePrice"];
export type Terms = components["schemas"]["SaleTerms"];
export type Models = Record<string, Terms>;
export const RATE_FIELDS = [
  ["inputPricePerMillion", "Input credits per million"],
  ["outputPricePerMillion", "Output credits per million"],
  ["cacheReadPricePerMillion", "Cache read credits per million"],
  ["cacheWrite5mPricePerMillion", "Cache write 5m credits per million"],
  ["cacheWrite1hPricePerMillion", "Cache write 1h credits per million"],
  ["reasoningPricePerMillion", "Reasoning credits per million"],
  ["costPerRequest", "Credits per request"],
] as const;
export type Rates = Record<(typeof RATE_FIELDS)[number][0], string>;
export type PriceDraft = Rates & { maxOutputTokens: string };
export type RuleDraft = { serviceTier: string; minInputTokens: string; maxInputTokens: string; price: PriceDraft };
export type ModelDraft = { model: string; mode: NonNullable<Price["mode"]>; discount: string; price: PriceDraft; rules: RuleDraft[] };

export function whole(raw: string, label: string, maximum = Number.MAX_SAFE_INTEGER): number {
  if (!/^\d+$/.test(raw) || raw.length > 16 || BigInt(raw) > BigInt(maximum)) throw new Error(`${label} must be a whole number from 0 to ${maximum}`);
  return Number(raw);
}
export function creditsText(minor: number | null | undefined): string {
  if (minor == null) return "";
  if (!Number.isSafeInteger(minor) || minor < 0) throw new Error("Stored amount exceeds this editor's exact range");
  const n = BigInt(minor);
  return `${n / 1000000n}.${String(n % 1000000n).padStart(6, "0")}`;
}
export function priceDraft(price?: components["schemas"]["SalePriceSet"]): PriceDraft {
  return { inputPricePerMillion: creditsText(price?.inputPricePerMillion ?? 0),
    outputPricePerMillion: creditsText(price?.outputPricePerMillion ?? 0),
    maxOutputTokens: String(price?.maxOutputTokens ?? 1024),
    cacheReadPricePerMillion: creditsText(price?.cacheReadPricePerMillion),
    cacheWrite5mPricePerMillion: creditsText(price?.cacheWrite5mPricePerMillion),
    cacheWrite1hPricePerMillion: creditsText(price?.cacheWrite1hPricePerMillion),
    reasoningPricePerMillion: creditsText(price?.reasoningPricePerMillion), costPerRequest: creditsText(price?.costPerRequest) };
}
export function modelDraft(model = "", terms?: Terms): ModelDraft {
  return { model, mode: terms?.price.mode ?? "chat", discount: String(terms?.discountPercent ?? 0),
    price: priceDraft(terms?.price), rules: (terms?.price.rules ?? []).map((r) => ({
      serviceTier: r.match.serviceTier ?? "", minInputTokens: r.match.minInputTokens == null ? "" : String(r.match.minInputTokens),
      maxInputTokens: r.match.maxInputTokens == null ? "" : String(r.match.maxInputTokens), price: priceDraft(r.price) })) };
}
function priceValue(draft: PriceDraft, mode: ModelDraft["mode"]): components["schemas"]["SalePriceSet"] {
  const result: components["schemas"]["SalePriceSet"] = { inputPricePerMillion: 0 };
  for (const [key, label] of RATE_FIELDS) {
    if (!draft[key].trim() && key !== "inputPricePerMillion" && key !== "outputPricePerMillion") continue;
    const value = decimalMinor(draft[key], 6, true);
    if (value == null) throw new Error(`${label}: use an exact nonnegative amount with at most six fractional digits`);
    result[key] = value;
  }
  result.maxOutputTokens = whole(draft.maxOutputTokens, "Maximum output tokens");
  if (mode === "chat" && result.maxOutputTokens === 0) throw new Error("Chat maximum output must be positive");
  if (mode !== "chat" && result.maxOutputTokens !== 0) throw new Error("Non-chat maximum output must be zero");
  if ((mode === "embeddings" || mode === "rerank") && result.outputPricePerMillion !== 0) throw new Error("Input-only modes require zero output price");
  return result;
}
export function termsValue(draft: ModelDraft): Terms {
  const rules = draft.rules.map((rule) => {
    const match: components["schemas"]["SalePriceMatch"] = {};
    if (rule.serviceTier.trim()) match.serviceTier = rule.serviceTier.trim();
    if (rule.minInputTokens) match.minInputTokens = whole(rule.minInputTokens, "Minimum input tokens");
    if (rule.maxInputTokens) match.maxInputTokens = whole(rule.maxInputTokens, "Maximum input tokens");
    if (!Object.keys(match).length) throw new Error("Each conditional rate needs a tier or token bound");
    if (match.minInputTokens != null && match.maxInputTokens != null && match.minInputTokens > match.maxInputTokens) throw new Error("Conditional minimum exceeds maximum");
    return { match, price: priceValue(rule.price, draft.mode) };
  });
  if (rules.length > 100) throw new Error("At most 100 conditional rates are supported");
  return { price: { ...priceValue(draft.price, draft.mode), mode: draft.mode, rules }, discountPercent: whole(draft.discount, "Discount", 100) };
}
export function modelsValue(drafts: ModelDraft[]): Models {
  if (drafts.length < 1 || drafts.length > 500) throw new Error("A book requires 1 to 500 models");
  const result: Models = Object.create(null);
  for (const row of drafts) {
    const name = row.model.trim();
    if (!name || name.length > 200 || Object.hasOwn(result, name)) throw new Error("Model names must be nonempty and unique, at most 200 characters");
    result[name] = termsValue(row);
  }
  return result;
}
/** Text is explicitly UTC; never reinterpret a local wall-clock time. */
export function utc(raw: string): string | null {
  if (!raw.trim()) return null;
  const text = raw.trim();
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,6})?Z$/.test(text) || !Number.isFinite(Date.parse(text))) throw new Error("Use a UTC timestamp such as 2026-12-01T00:00:00Z");
  const result = new Date(text).toISOString();
  if (result.slice(0, 19) !== text.slice(0, 19)) throw new Error("Invalid UTC calendar date");
  return /\.\d{4,6}Z$/.test(text) ? text : result;
}
/** Preserve the API's microsecond boundaries even though JS Date uses milliseconds. */
export function utcMicros(raw: string): bigint {
  const text = utc(raw);
  if (!text) throw new Error("A UTC timestamp is required");
  const fraction = text.match(/\.(\d+)Z$/)?.[1] ?? "";
  return BigInt(Date.parse(text.slice(0, 19) + "Z")) * 1000n + BigInt(fraction.padEnd(6, "0"));
}
export function scheduleState(row: { effectiveFrom: string; effectiveUntil: string | null }, now = Date.now()): string {
  const start = utcMicros(row.effectiveFrom);
  const end = row.effectiveUntil ? utcMicros(row.effectiveUntil) : null;
  const current = BigInt(now) * 1000n;
  if (end != null && end <= start) return "Canceled";
  if (end != null && end <= current) return "Ended";
  return start > current ? "Scheduled" : "Active";
}
