# Unrouteable manual receipt refund regression (#966)

Date: 2026-11-10. Parent #946 / N10. No real money moved.

## Actual browser reproduction

The owner recorded a clearly labeled local fixture manual receipt of 25 USD
minor at the persisted 0.250001 USD/credit conversion. Order
`c969f830-9a3f-41a1-b16f-a50ad2464d50` fulfilled **999,996** credit minor.
The explicit free-price override was ended through the sale schedule UI. A
customer Chat turn through the local SSE model returned 9 input / 3 output tokens
and settled **40** minor under the published 3.000007 / 4.000009 per-million
rates. The wallet showed **999,956** available and zero holds. The attached
balance and screenshot capture this conservation check.

The customer then requested the full order refund and the Finance user approved.
Refund `fdef6fb1-116d-450a-a1a6-14f702727cea` committed `provider_pending` and
credit note `ee78f31a-6f8d-4426-9c20-167693ccdaff`. The HTTP provider half failed
while decoding null `merchant_id`; the real background retry job repeated the
same failure. Manual receipts have no merchant or provider payment. There was
no cash effect and no bank or PSP refund, despite the pending provider state.
The approved amount is an order reservation, not a wallet hold. Source-specific
cash allocation is separately tested for actual provider returns.

## Repair boundary

New manual or incomplete-route requests and approvals fail before effects.
Existing requests remain readable and can be declined before approval. Retained
pending rows remain unresolved for explicit financial review, but every retry
entry excludes them before spending an attempt or decoding provider coordinates.
Nothing changes historical approval, note or cash facts.

The customer UI explains the out-of-band reconciliation boundary in both locales
and offers no automated manual refund action. Finance resolve errors remain
visible on the page after its confirmation dialog closes.

## Validation

- Eleven core refund tests and eleven HTTP refund tests passed with zero skips.
  New regressions cover request refusal/no rows, null refundable offer, original
  request replay, approval refusal/no note, decline, retained pending reads,
  inert target/claim/due-list, zero provider calls and foreign-customer 404.
- Strict core/server all-target Clippy passed in 3m24s.
- Contract/codegen, frontend build and pinned formatting passed.
- The initial workspace run stopped after 794 passes at the old manual bundle
  upgrade fixture, which still expected a new automatic refund on an order without
  a provider route. #972 retains a pre-upgrade request/replay and explicitly checks
  new-request refusal, original order/fulfillment and unchanged purchased/promo
  funds. All 12 focused upgrade/refund tests pass. The no-fail-fast full run
  completed on base `cf422dc8` plus the #972 fixture repair: **2,256 passed**,
  zero failures, six existing opt-in tests skipped (2,313.809 seconds; run
  `62b9dc44-9ece-45f4-afc7-57126499cde7`). This includes the integrated
  jsonwebtoken 9 repair; newer dependency changes retain their own CI evidence.
- Browser English/Chinese boundary text was observed using the rebuilt frontend
  on the original integrated backend. This checks UI behavior; the new backend
  refusal is demonstrated by the actual PostgreSQL/HTTP regression above.

Baseline backend source `ac7bb191ee7a2b52af4a23f2ce54cd9c6f9d82e5`, binary
SHA-256 `941e9f1776e64ed5559074c637bf73d67de7380a382dc12f56e1b149ca610c53`.
Frontend base `879e6e6f` plus this repair, byte inventory attached. These are
local development facts, not candidate, genuine PSP or money-out certification.

## Aggregate test layout continuation

After main `2ee70601` consolidated integration binaries, the unchanged repair
was merged onto that layout. The exact `bundle_upgrade::` PostgreSQL regression
passed (1/1), core all-target strict Clippy and pinned formatting passed. The
original 2,256-pass evidence remains scoped to its recorded development tree;
it is not relabeled as a new main/candidate run. Main's duplicate issue #1002
is the same failure. The independent obsolete CI-selector regression is #1003.
