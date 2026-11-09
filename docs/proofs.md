# The proof surface map

One row per proof surface: what it establishes, and the sentence it must not be read as. The
product's discipline is the right-hand column — oxsum never claims "trustless", and this page is
where that discipline is auditable.

The entry description preserves complete composed evidence up to 16,384 Unicode
scalar values (at most 65,536 UTF-8 bytes before transport escaping, #873).
The capacity increase preserves v14 settlement bytes and canonical hashes, so
old proof bytes and hashes continue to verify. New gateway writes use v15 for
complete reason commitments and explicit guardrail phases. A verifier built with the former 512-character
limit cannot decode a longer new entry; distribute the matching native/browser
verifier with the server. The gateway separately preflights a complete 16,384-byte
JSON envelope before execution under the [bounded evidence contract](gateway-evidence-budget.md)
(#875); the storage limit alone does not certify arbitrary policy output sizes.

| Surface | What it proves | What it does not prove |
| --- | --- | --- |
| Bills retained-head check | local WASM authenticates the original/current signatures under the retained key and verifies the consistency proof against the retained root; equal heads match exactly | the first server-supplied key or newly served verifier is independently trusted; an empty head establishes prior history; token counts or all bill arithmetic/completeness were checked |
| `oxsum_verify::verify_bundle` | the entry bytes hash to the independently retained expected hash and its inclusion proof reaches the supplied head | the head's signature, the operator's identity, arithmetic or completeness; signatures and consistency are separate checks |
| `verify_bill` / SDK `verifyBill` / `/verify` | entry inclusion plus the supported charge check, with separate price/sale-binding sentences | `ok: true` alone does not mean the price/sale binding verified: read those sentences, including absent/invalid bindings and old/new-schema arithmetic skips. Operator head trust is checked separately |
| `oxsum_verify::verify_charge` (settlement descriptions, schema v6+) | the recorded charge recomputes from the metered usage and the priced lines inside the entry; a schema the verifier does not know reports inclusion only, with the reason named | that upstream really returned those tokens — the count is what the writer recorded, and your own response log is the counter-check |
| The v10 `cache` block on a hit's settlement (issue #497) | the charge is a replay: `sourceEntryId` names the ledger entry the answer was produced under, `similarity` is the honest score (`1` for the exact-digest store this build runs), `upstreamAttempts: 0` says no upstream call happened, and the input repriced as `cache_read` recomputes like every other v3+ line binding | that the original entry's answer was correct for your question — the block proves *which* turn's answer you paid a cache-read price for, not that the question was the same; the prompt digest lives in the store row, not the bill |
| The v11 `guardrail` array on a settlement (issue #502) | each entry `{policy, verdict, reason, streamed?}` names a configured policy that acted on the turn and how — `flagged` settled normally, `blocked` carries `charged: 0` on a post-call verdict (the refunded hold's own record) — so the bill itself says which rule judged it | that the verdict was *right* — the array is the rule's own record, not a finding that the policy's judgment was correct; and a `streamed: true` entry is honest about the limit: the bytes were already out, so the verdict is a record, not a refusal |
| The v12 `requestedModel`/`servedModel` pair on a settlement (issue #500) | the bill names the model that answered — and the model the caller asked for when a request failed over across models under its configured `oxsum.model_fallbacks` chain. `servedModel` is what priced the turn (`model` means the same thing for back-compat readers); the price binding is still the `lines`/`usage` recomputation below | that the fallback was a *good* answer — the pair proves which model name served and was billed under, not that the answer was correct; `requestedModel` is recorded, never a pricing input |
| The v13 `experiment` block on a settlement (issue #509) | the turn served under the experiment's weighted pick: `{id, variant}` names the `oxsum.experiments` row and which variant's model answered, so the bill groups spend by the experiment's arm — the served model is still what priced the turn, and `servedModel`/`model` name it as always | that the variant's answers were *better* — the block proves which arm of the experiment the caller's request ran under, not which arm's output was preferable; scoring stays the customer's own eval on the responses they hold, never the bill's judgment |
| The signed tree head (`GET /api/v1/log/head`, key from `/api/v1/log/key`) | this is the operator's attestation of the log's state at a size and root | that the head is current (ask for a newer head and a consistency proof), or that the operator runs only one history — comparing independently retained or witnessed heads can expose a fork |
| `GET /api/v1/log/consistency?from=` | the newer head provably extends the older head you archived | that nothing was withheld before the older head — an append-only log can still omit, which is why completeness is stated, not proven |
| Witness cosignatures (`verify_witnesses`, `GET /api/v1/log/checkpoint`, `OXSUM_WITNESS_KEYS`, the bill page) | the holder of a pinned witness key cosigned this head, and the signature verifies under that key — the dashboard's log page and the customer's bill page run the same check | that the witness is organizationally independent, audited every row, does not collude, or belongs in the trusted pinset — a verifier may substitute its own; and on the bill page, "no cosignature covers this head yet" is a true answer, not a failure |
| The price binding (`price` in a bundle, `GET /api/v1/log/price-head`) | the catalog row this bill charged under is the row the operator attested, at that version | that the price was fair or that no other price ever existed; it binds one row, not the whole catalog |
| The sale binding (`sale` in a bundle, `GET /api/v1/log/sale-head`, issue #600) | the *customer* sale terms this bill charged under — the resolved `SalePriceSnapshot` a v14 settlement's `sale.digest` commits — are the snapshot the operator attested: the customer, model, book/version/draft/legacy provenance, effective window and sale price plus discount, all inside the digest. Supplier `upstream` rates are never part of a sale snapshot | that a wholesale-priced bill proves its price this way — a record committing no `sale` digest reports `NotSalePriced`, and an old channel checkpoint cannot answer for customer terms; nor that the terms were fair, only that they are the ones attested |
| `oxsum_verify::verify_usage_report` (the report bundle, `/verify`, the bill page) | every shown settlement is in the organization's ledger, the shown rows fold to the subset digest the marker commits, the marker is in the supplied head, and a present note verifies under the supplied trusted key; absent notes prove digests/inclusion only. Over an anchored span, also that no day or row of the customer's log is missing (the head is rebuilt from every day leaf, each leaf from its rows) | per-row charge arithmetic (that is `verify_charge`), and — when the bundle carries no `completeness` — completeness itself: the entry count and total are then the organization's signed claim |
| The bill page's browser check (`/bill/{token}`) | the same report checks, run in the customer's browser over the bundle the page embeds, with the verifier's hash shown; the anchored-span line appears when the proof is present | write-time honesty: retained hashes/heads detect conflicting history, not whether the writer counted what the model returned; and completeness for unanchored spans |
| `oxsum_verify::verify_statement_anchor` (the statement's anchor, `GET /api/v1/statements/{id}/proof`) | the covered window and statement rollup match their committed digests; a present note attests the expected origin/head at finalization. Unsigned anchors prove digests only. The check payload supplies the ordered hashes and expected head; validate its origin and key against retained trust inputs | that a printed or exported PDF was not edited afterwards — the anchor covers the figures, not the rendering |
| `oxsum_verify::verify_evidence_bundle` (the v2 evidence bundle, `POST /api/v1/org/evidence-bundles`, `GET /api/v1/org/statements/{id}/evidence`, `GET /api/v1/evidence/{token}`, issue #616) | every artifact the manifest names is served and re-digests to the figure the ledger's marker bound (`oxsum/evidence-bundle/v1`), the manifest itself re-digests to the committed `manifestDigest`, the money artifacts' ledger entries adopt-verify and include in the supplied head, and a present note verifies under the selected operator key; absent notes prove digests/inclusion only. A period bundle commits the whole closed span (usage records, plan statements, invoice journals, payment orders, refunds, disputes, credit notes, day seals); a statement bundle commits one document | completeness of what the scope block spells out: the platform's own `upstreamCostFigures` (a different organization's books, redacted unless `OXSUM_EVIDENCE_UPSTREAM_COST` opts in), another organization's rows, history outside the span, and in-flight operations. It proves the committed set, not that the operator counted tokens honestly |
| The outbound webhook signature (`request.settled`, `deposit.credited`, …) | the delivery was signed by this deployment | that the payload is correct or complete — the signature says who spoke, not what is true |

## Retain independent trust inputs

A proof can detect altered bytes relative to an expected hash, and a consistency
proof can detect a history that fails to extend the head a reader retained.
Signed heads and independently operated witnesses provide attestations that can
expose equivocation when compared. A replacement database plus newly supplied
keys and hashes cannot establish continuity with a history the reader never kept.
A second process or database under the same operator does not establish witness
independence.

Recorded usage and charge arithmetic are checkable against the entry. Whether
the record matches the actual upstream response remains a claim; retain your own
request/response log for comparison. Each verification surface composes only the
checks it documents. In particular, report verification does not run every
individual settlement's arithmetic check, and an unsigned result must not be
presented as an authenticated operator attestation.

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


### Versioned plan recognition evidence (#838)

A new v2 plan-statement artifact commits the policy number, immutable original version/authorization/fee/service bounds, capture time and listed accounting effects. Each effect names its actual official ledger entry (zero-money days have none); the bundle carries those entries and the pure verifier rejects an omitted referenced entry. Policy inputs/effects and statement itemization are collected from one repeatable-read database snapshot. Existing bundles and v1 verification remain readable and byte-stable.

This is source integrity and ledger inclusion for the captured list. It does not independently attest actual service delivery, statutory accounting compliance, completeness of effects absent from the captured list, or financial changes after mint. A statement may export before service ends; its committed scope explicitly excludes future recognition and later adjustments. Re-minting its immutable scope returns the same bytes.


Evidence minting freezes exact artifact inputs durably in `evidence_mint_inputs`
before appending its deterministic scope marker. An atomic unique scope insert selects
the winning snapshot; concurrent mints reload it and return the persisted descriptor.
No database connection is retained across nested ledger calls.
If a process fails after marker append but before exported bytes persist, retry
uses those frozen inputs even when later refunds or daily recognition have landed.
An older orphan marker with no matching captured inputs explicitly refuses
publication rather than serving an export bound to a different digest. Existing
stored bundles remain unchanged; independent ledger verification still requires
the retained trust inputs and stays within the documented proof scope.

Version-15 settlements may commit a long reason by SHA-256 and UTF-8 byte length
instead of carrying its full text. The entry hash/inclusion binds the commitment.
Only comparison with the exact separately supplied full UTF-8 reason establishes
that text matches; a bundle lacking it makes no such claim. Neither a digest nor
a matched reason certifies policy semantics or truthful provider output. Earlier
v3–v14 entries retain their original verification rules.

Statement repayment (#777) retains exact order/statement/journal allocation and
return links beside canonical entry hashes. The finance evidence route validates
recorded entry shape and original allocation limits. Reconciliation snapshots
also compare those effects and the exact customer document lines; financial
voids retain per-payment payable ownership. These records prove recorded source
relationships within the existing operator/ledger trust scope. They do not
independently prove external provider settlement, fair collection, delivery of
service, or events after a captured artifact was minted.
### Application downloads of retained evidence (#924)

Application `evidenceRead` serves the same retained version-2 bytes after explicit
namespace/customer authorization and internal consistency checks. It does not
create an anchor or establish completeness beyond the original manifest scope.
The retained key is not an independent trust root: consumers must still use
previously held origin/key/digest and the existing verifier. Downloads refuse
cost-inclusive, malformed, duplicate-key or oversized files instead of rewriting
the committed content. Descriptor listing alone proves nothing about file validity.
