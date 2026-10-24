# oxsum — a verifiable AI credit wallet

**The short version.** oxsum is an AI gateway you run yourself, with billing that
your customers can check. Point an OpenAI or Anthropic SDK at it instead of at
the provider; every call reserves an upper bound *before* a token is generated,
then settles against the usage the upstream actually reported. Every charge —
top-up, settled turn, adjustment — is one entry in an append-only Merkle log the
organization can export and verify in a browser, without trusting the server
that issued it.

It is a portfolio piece and a working system, not a hosted service: the README's
quick start brings the whole thing up against a local PostgreSQL, and the same
single binary is what a deployment runs.

## Who it is for

Two readers, with opposite seats at the same table:

- **A reseller or platform** that meters LLM usage for its own customers. The
  pain oxsum answers is the invoice question — *"where did this number come
  from?"* — which today has exactly one answer, "trust us". On oxsum the answer
  is a bill the customer checks themselves, entry by entry, in their own
  browser.
- **An engineer evaluating the codebase.** The interesting claims — concurrent
  requests cannot overdraw, retries cannot double-charge, a rewritten history is
  detectable — are not slogans. They are properties the ledger enforces and the
  test suite pins, and this page names where each one lives.

## The three-part trust model

Every billed turn walks the same three steps, and each step answers the
objection the step before it leaves open:

1. **Freeze before the call.** When a request arrives, oxsum computes the most
   it could possibly cost — the byte-count bound on the input, the caller's
   `max_tokens` on the output, priced at the dearest applicable rate — and
   freezes that much against the organization's balance. If the balance cannot
   cover it, the request is refused *before upstream sees it*, and the refusal
   itself becomes a ledger row. A request can never spend more than was
   reserved; there is no "the model ran long, so the bill did too".
2. **Settle to actual.** While the answer streams, oxsum reads the token counts
   the upstream reports in the final chunk and settles the real charge — always
   ≤ the freeze, with the difference refunded. An upstream that errors before
   emitting anything costs zero; an unreachable upstream costs zero; a client
   that disconnects pays only for what was already forwarded. The charge, the
   token counts and the exact price version are packed into the settlement
   entry itself, so "what was I billed for" is answered by the record, not by
   the server's memory of it.
3. **Verify the bill yourself.** The ledger is append-only and every entry is
   hashed into a Merkle tree whose head the operator signs. A bill comes with a
   proof bundle — the entry, its inclusion proof, the signed head — and
   `/verify` recomputes the content hash and walks the proof in the customer's
   own browser. The verdict does not come from the server that issued the bill;
   it comes from the verifier's bytes, which travel to the browser rather than
   the customer trusting a rendered page.

![The whole flow in one pass — top up, billed turns, the ledger, a browser-side verification](assets/demo.gif)

## What a bill actually is

The demo script drives the flow end to end against a scripted upstream — no real
provider, no API key — and prints the artifacts a user holds:

```
== 1. Register and top up ==
   topped up 100000000 minor units (100 credits)
   top-up entry: 7dcf20b9-…   content hash (keep for verification): 45b88ddd…

== 2. One billed chat turn through the OpenAI SDK ==
   upstream said: "Holds are oxsum's way of saying 'reserved'."
   x-oxsum-request-id: fca1ed85-…

== 3. The turn's settlement entry and its proof ==
   billing record: {"request": "fca1ed85-…", "channel": "demo-…",
     "model": "demo-chat", "priceVersion": 1, "kind": "usage",
     "inputTokens": 23, "outputTokens": 11, "charged": 34, "freeze": 118}
   balance after the turn: 99999966 minor units (charged 34)
```

The settlement entry is the bill: it names the request, the model, the price
version that priced it, the token counts upstream reported and the freeze it was
charged against. `verify_charge` recomputes the recorded charge from the usage
and rates inside the entry — so the record proves not just that *something* was
written, but that the arithmetic inside it holds. The bundle a row of the bills
page links to is the same JSON `/verify` checks:

| | |
| --- | --- |
| ![Top up through the Hyperswitch checkout](assets/topup-hyperswitch.png) | ![Bills — every row with its verify/proof actions](assets/bills.png) |
| ![Usage aggregated per key](assets/usage.png) | ![The /verify verdict](assets/verify.png) |

## What is proven, and what is claimed

The product's discipline is that it never says *trustless*. What a verified bill
establishes, in its own words:

- **Proven** — this entry was written to this ledger; its bytes hash to the
  recorded content hash; the head carrying it is signed by the operator key; a
  versioned settlement's recorded charge recomputes from the usage and priced
  lines inside the entry; a price the bill names is bound to a catalog row the
  operator attested; and, over an anchored span, a usage report carries every
  settlement of the customer it covers — a dropped day or a misattributed row
  fails the check.
- **Still a claim** — that upstream really returned that many tokens (the
  record proves what was written, not what the model produced; a customer who
  kept their own request log is the counter-check), and completeness over an
  *unanchored* span, where "these are all the settlements" is the
  organization's signed statement rather than a Merkle fact.

[proofs.md](proofs.md) is the surface-by-surface map — every proof surface and
the sentence it must not be read as.

## Beside hosted billing

A hosted billing service (a Stripe usage meter, a managed gateway) is the right
choice when the invoice is the vendor's problem. oxsum is for the operator who
wants the billing to be *evidence*: the ledger lives in their own database, the
proofs are checkable without oxsum running, and a customer's verification runs
in the customer's browser. The trade is operational — one binary plus
PostgreSQL, a Stripe/Airwallex/Hyperswitch/x402 rail for real top-ups, and the
operator carries the deploy.

## Where to go next

- Run it: the [README](../README.md) quick start, then `demo/demo.py` for the
  scripted end-to-end pass.
- The behavior spec: [product.md](product.md) — every page and flow, each marked
  shipped or planned.
- The checkable surface: [proofs.md](proofs.md) and
  [verify-your-bill.md](verify-your-bill.md).
