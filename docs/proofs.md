# The proof surface map

One row per proof surface: what it establishes, and the sentence it must not be read as. The
product's discipline is the right-hand column — oxsum never claims "trustless", and this page is
where that discipline is auditable.

| Surface | What it proves | What it does not prove |
| --- | --- | --- |
| `oxsum_verify::verify_bundle` (`/verify`, the browser's `verify.js`) | this entry was written to this ledger, its bytes hash to the recorded content hash, and the head that carries it is signed by the operator key | that the operator's key is the one you should trust — that is a trust choice you make against a key you obtained elsewhere; that the ledger holds everything (completeness is the organization's claim) |
| `oxsum_verify::verify_charge` (settlement descriptions, schema v6+) | the recorded charge recomputes from the metered usage and the priced lines inside the entry; a schema the verifier does not know reports inclusion only, with the reason named | that upstream really returned those tokens — the count is what the writer recorded, and your own response log is the counter-check |
| The signed tree head (`GET /api/v1/log/key`, the bundle's note) | this is the operator's attestation of the log's state at a size and root | that the head is current (ask for a newer head and a consistency proof), or that the operator runs only one history — witnesses cover that |
| `GET /api/v1/log/consistency?from=` | the newer head provably extends the older head you archived | that nothing was withheld before the older head — an append-only log can still omit, which is why completeness is stated, not proven |
| Witness cosignatures (`verify_witnesses`, `GET /api/v1/log/checkpoint`, `OXSUM_WITNESS_KEYS`, the bill page) | an independent witness saw this head, and its cosignature verifies under the key the deployment pins — the dashboard's log page and the customer's bill page run the same check | that the witness audited every row, that witnesses do not collude, or that the pinset is the right set of witnesses — a verifier may substitute its own; and on the bill page, "no cosignature covers this head yet" is a true answer, not a failure |
| The price binding (`price` in a bundle, `GET /api/v1/log/price-head`) | the catalog row this bill charged under is the row the operator attested, at that version | that the price was fair or that no other price ever existed; it binds one row, not the whole catalog |
| `oxsum_verify::verify_usage_report` (the report bundle, `/verify`, the bill page) | every shown settlement is in the organization's ledger, the shown rows fold to the subset digest the marker commits, the marker is in the ledger, and the head is signed; over an anchored span, also that no day or row of the customer's log is missing (the head is rebuilt from every day leaf, each leaf from its rows) | per-row charge arithmetic (that is `verify_charge`), and — when the bundle carries no `completeness` — completeness itself: the entry count and total are then the organization's signed claim |
| The bill page's browser check (`/bill/{token}`) | the same report checks, run in the customer's browser over the bundle the page embeds, with the verifier's hash shown; the anchored-span line appears when the proof is present | write-time honesty: a ledger can prove it was not rewritten, not that the writer counted what the model returned; and completeness for unanchored spans |
| `oxsum_verify::verify_statement_anchor` (the statement's anchor, `GET /api/v1/statements/{id}/proof`) | the statement's window hashes match the ledger and the signed head at finalization; the bundle serves the covered hashes and the head that follows the anchor, so the verifier runs off the answer alone | that a printed or exported PDF was not edited afterwards — the anchor covers the figures, not the rendering |
| The outbound webhook signature (`request.settled`, `deposit.credited`, …) | the delivery was signed by this deployment | that the payload is correct or complete — the signature says who spoke, not what is true |

## The two claims the product makes

- **The operator cannot rewrite history.** A signed head, a consistency proof and, where configured,
  independent witness cosignatures make a rewritten or forked ledger detectable — by the
  organization, by its customers, and by anyone holding an archived head.
- **Your token counts match your own usage.** A settlement's charge recomputes from the usage
  recorded in the entry, and the record is bound to the ledger; a customer who kept their own
  request log can compare counts and totals against the bill.

What remains a claim rather than a proof is completeness **when the span is not anchored**: a
report attests "these are all the settlements for this customer over this span", and without the
anchored day log the bundle cannot enumerate rows the organization chose never to show. The
anchored proof (issues #337-#339, design in [decisions.md](decisions.md) 2026-10-22) now ships:
the daily anchor fixes each customer's day-log head in the ledger, the report carries every day
leaf, and the verifier rebuilds each leaf from its rows — a dropped day or a misattributed row
fails. What even it cannot prove is write-time honesty: whether the organization counted the
tokens the model returned. A day the anchor fixed is sealed — a settle that would book into it is
refused with `409 DAY_SEALED` — so the day log is append-only by rule, and `anchored_day_altered`
in the reconciliation report catches any leaf that changed anyway. Unanchored history keeps the
older, weaker statement.

The customer-facing version of this page is [Verifying an oxsum bill](verify-your-bill.md); the
operator-facing surfaces are described in [product.md](product.md). The wire-level companion —
every object and check computed field-by-field — is [the transparency-log spec](transparency-log-v1.md).
