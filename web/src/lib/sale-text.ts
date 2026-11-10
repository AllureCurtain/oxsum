import type { TFunction } from "i18next";
import { SaleInputError } from "./sale-input-error.ts";

const labels: Record<string, string> = {
  "Discount": "discount",
  "Maximum output tokens": "maximumOutput",
  "Minimum input tokens": "minimumInput",
  "Maximum input tokens": "maximumInput",
  "Input credits per million": "inputRate",
  "Output credits per million": "outputRate",
  "Cache read credits per million": "cacheReadRate",
  "Cache write 5m credits per million": "cacheWrite5mRate",
  "Cache write 1h credits per million": "cacheWrite1hRate",
  "Reasoning credits per million": "reasoningRate",
  "Credits per request": "requestRate",
  "Input tokens / event units": "inputCount",
  "Output tokens": "outputCount",
  "Cached input tokens": "cachedCount",
  "Cache write 5m tokens": "write5mCount",
  "Cache write 1h tokens": "write1hCount",
  "Reasoning tokens": "reasoningCount",
  "Active": "active",
  "Scheduled": "scheduled",
  "Ended": "ended",
  "Canceled": "canceled",
  "default": "default",
  "assigned": "assigned",
  "override": "override",
  "draft": "draft",
  "legacy": "legacy",
  "input": "input",
  "output": "output",
  "cache_read": "cacheRead",
  "cache_write_5m": "cacheWrite5m",
  "cache_write_1h": "cacheWrite1h",
  "reasoning": "reasoning",
  "request": "request",
  "chat": "chat",
  "embeddings": "embeddings",
  "rerank": "rerank",
  "event": "event",
  "active": "customerActive",
  "suspended": "customerSuspended",
  "archived": "customerArchived"
};
const serverErrors: Record<string, string> = {
  "overlapping sale windows in the same scope": "overlap",
  "draft revision changed": "staleRevision",
  "draft revision is already published": "alreadyPublished"
};

/** Translate known display labels while preserving unknown protocol values. */
export function saleLabel(label: string, t: TFunction): string {
  return labels[label] ? t(`sale.${labels[label]}`) : label;
}

/** Preserve unknown server diagnostics; local validators carry typed metadata. */
export function saleError(error: unknown, t: TFunction, fallback: string): string {
  if (error instanceof SaleInputError) {
    const values = { ...error.values };
    if (typeof values.label === "string") values.label = saleLabel(values.label, t);
    return t(`sale.${error.key}`, values);
  }
  if (!(error instanceof Error)) return fallback;
  return serverErrors[error.message] ? t(`sale.${serverErrors[error.message]}`) : error.message;
}
