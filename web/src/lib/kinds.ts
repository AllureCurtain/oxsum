// Plain-language labels for the ledger's kind columns (issue #310).
//
// The settlement labels mirror `outcome_label` in `crates/web/src/bill.rs`: the
// public bill page and the dashboard must say the same thing about the same
// kind. Keep the two in step when a kind is added — the bill page's no-JS floor
// and this map are the two places a customer reads them.

export interface Outcome {
  label: string;
  detail: string;
}

const OUTCOMES: Record<string, Outcome> = {
  usage: {
    label: "settled",
    detail: "upstream reported usage and it was priced",
  },
  estimated: {
    label: "estimated",
    detail: "upstream reported no usage; the turn was priced from a local estimate",
  },
  client_cancelled: {
    label: "client cancelled",
    detail: "the client disconnected mid-stream; the forwarded part was priced",
  },
  upstream_error: {
    label: "upstream error",
    detail: "upstream answered with an error before emitting anything — nothing charged",
  },
  upstream_unreachable: {
    label: "upstream unreachable",
    detail: "upstream could not be reached — nothing charged",
  },
  capped: {
    label: "capped at freeze",
    detail: "upstream's usage priced above the frozen upper bound; the freeze was charged",
  },
  swept: {
    label: "hold timed out",
    detail: "the hold timed out with no settlement and the sweeper released it — nothing charged",
  },
  unpriced: {
    label: "partly unpriced",
    detail: "a usage dimension the price book cannot cover; the rest was flagged, not billed",
  },
  released: {
    label: "released",
    detail: "the reporting service let the hold go — nothing charged",
  },
  refused: {
    label: "refused",
    detail: "admission refused the request before a hold — nothing charged",
  },
};

/// The label and explanation for a settlement kind; a kind without a value (a
/// live turn that has not settled) answers a dash, and an unknown kind answers
/// itself, so a value this build predates still renders.
export function settlementOutcome(kind: string | null | undefined): Outcome {
  if (!kind) {
    return { label: "—", detail: "the record carries no settlement kind yet" };
  }
  return (
    OUTCOMES[kind] ?? {
      label: kind,
      detail: kind.split("_").join(" "),
    }
  );
}

const ENTRY_KINDS: Record<string, string> = {
  topUp: "top-up",
  adjustment: "adjustment",
  settlement: "settlement",
};

/// The label for a ledger entry kind (`topUp`, `adjustment`, `settlement`).
export function entryKind(kind: string): string {
  return ENTRY_KINDS[kind] ?? kind;
}
