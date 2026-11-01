// Plain-language labels for the ledger's kind columns (issue #310).
//
// The settlement labels mirror `outcome_label` in `crates/web/src/bill.rs`: the
// public bill page and the dashboard must say the same thing about the same
// kind. Keep the two in step when a kind is added — the bill page's no-JS floor
// and this map are the two places a customer reads them. The strings here are
// translated through the `settlement.*` locale keys; callers pass `t`.

import type { TFunction } from "i18next";

export interface Outcome {
  label: string;
  detail: string;
}

// kind → the two locale keys under `settlement` (label + detail).
const OUTCOME_KEYS: Record<string, [string, string]> = {
  usage: ["usageLabel", "usageDetail"],
  estimated: ["estimatedLabel", "estimatedDetail"],
  client_cancelled: ["clientCancelledLabel", "clientCancelledDetail"],
  upstream_error: ["upstreamErrorLabel", "upstreamErrorDetail"],
  upstream_unreachable: ["upstreamUnreachableLabel", "upstreamUnreachableDetail"],
  capped: ["cappedLabel", "cappedDetail"],
  swept: ["sweptLabel", "sweptDetail"],
  unpriced: ["unpricedLabel", "unpricedDetail"],
  released: ["releasedLabel", "releasedDetail"],
  refused: ["refusedLabel", "refusedDetail"],
};

/// The label and explanation for a settlement kind; a kind without a value (a
/// live turn that has not settled) answers a dash, and an unknown kind answers
/// itself, so a value this build predates still renders.
export function settlementOutcome(kind: string | null | undefined, t: TFunction): Outcome {
  if (!kind) {
    return { label: "—", detail: t("settlement.noKindDetail") };
  }
  const keys = OUTCOME_KEYS[kind];
  if (!keys) {
    return { label: kind, detail: kind.split("_").join(" ") };
  }
  return { label: t(`settlement.${keys[0]}`), detail: t(`settlement.${keys[1]}`) };
}

const ENTRY_KINDS: Record<string, string> = {
  topUp: "settlement.topUp",
  adjustment: "settlement.adjustment",
  settlement: "settlement.settlement",
};

/// The label for a ledger entry kind (`topUp`, `adjustment`, `settlement`).
export function entryKind(kind: string, t: TFunction): string {
  const key = ENTRY_KINDS[kind];
  return key ? t(key) : kind;
}
