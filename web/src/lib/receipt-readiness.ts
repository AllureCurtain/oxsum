import type { components } from "../api/schema";

type ReceiptReadiness = components["schemas"]["CustomerReceiptReadiness"];

/** Shared request/bill outcome; no report or recent operational log is needed. */
export function receiptReadinessState(query: {
  isPending: boolean;
  isError: boolean;
  error?: { status?: number } | null;
  data?: ReceiptReadiness;
}): "loading" | "error" | "blocked" | "done" | "todo" {
  if (query.isPending) return "loading";
  if (query.isError) return query.error?.status === 403 ? "blocked" : "error";
  if (query.data?.state === "ready" && query.data.receipt) return "done";
  if (query.data?.state === "incomplete" || query.data?.state === "ready") return "error";
  return "todo";
}

/** Keep proof navigation bound to a verified receipt returned by this session. */
export function receiptProofHref(data?: ReceiptReadiness): string | undefined {
  return data?.state === "ready" && data.receipt
    ? `/verify?entry=${encodeURIComponent(data.receipt.entryId)}`
    : undefined;
}
