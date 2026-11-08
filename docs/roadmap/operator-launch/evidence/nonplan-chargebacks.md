# Non-plan chargeback Cash and original funding (#840)

This forward repair follows #826's atomic dispute boundary. It is a child of
#829; #841 non-plan refunds and #842 historical wrong-direction chargebacks stay
open. No migration, PSP call or live financial correction is part of this change.

## Implemented boundary

- Actual Hyperswitch `dispute_id` normalization, with conflicting legacy alias
  refused. Explicit signed case ID, amount and matching pinned fiat currency. Partial
  amounts must map exactly to integer credit minor units; malformed/contradictory
  events retain rejected/review evidence without rewriting an existing case.
- Full `Cr Cash` for each authenticated loss, even when cumulative real losses
  exceed original source cash. Only original, retained Wallet funding may be
  debited; source reuse is capped. Later top-ups cannot replace consumed source.
- Exact confirmed/fulfilled order and deterministic original ledger entry,
  content hash, no reversal and positive settled USD Cash/Wallet/CreditLine
  legs. All later settled Wallet debits and current pending reservations reduce
  the conservative bound. Credit-repayment money stays review suspense.
- Any prior refund history or unsupported earlier chargeback denies Wallet
  take-back. New refund requests/approval and unperformed grants are fenced after
  non-plan loss. The source lock spans non-plan grant and immutable fulfillment.
  Its try-lock releases contending connections; a shared pre-checkout `Db` queue
  bounds grants, including distinct orders. Pools below three fail before grant;
  operator sizing still recommends ten or more. Independent wrappers over one
  pool must share/clone a `Db` handle to share the admission queue.
- Bundle source promotion closes in the loss transaction; held releases burn
  under the existing policy. Audit failure rolls back Cash, Wallet and promo.
- Audit retains full source/uncertainty and allocation, under
  `source.policy=nonplan-cash-v1`. This does not reopen old paid debt documents or
  certify/rewrite historical wrong-direction chargebacks.

## Development verification

The worktree uses an empty ignored `.env` plus explicit loopback PostgreSQL URLs;
no upstream or PSP configuration is inherited from the main workspace.

- 59 focused core checks passed, zero skipped, across `nonplan_chargebacks`,
  `refunds`, `dispute_atomicity`, `bundles`, `payment_orders` and `merchant_events`.
  Run `43e1b297-5dc6-4e69-9a14-a1cbc0ad426c`, 19.034 seconds after compilation.
- The final lost-source worker exclusion passed 27 further source/fulfillment
  checks, zero skipped: `26e39245-e492-4773-b95e-a8940074466a`, 5.631 seconds.
- Ten HTTP refund/dispute checks passed, zero skipped, including native
  `dispute_id`-only partial callbacks, conflicting aliases and replay:
  `d0d4c93b-cc8e-4af9-92fa-78002089667f`, 12.511 seconds.
- The source suite contains 19 PostgreSQL scenarios, including actual blocked
  backends, 16-caller same/different-order bursts with a three-connection pool,
  concurrent dispute refusal without connection pinning, cancellation and a
  two-connection pre-grant refusal. Official engine reversals test historical
  source and retained-effect refusal; no immutable ledger guards are disabled.
- All 16 integration library checks passed, zero skipped:
  `cc7c5fa0-90c3-4d02-83f5-e69623d9449f`. Native dispute normalization is also
  covered through the signed HTTP fixture above.
- Four API credential-contract checks, regenerated TypeScript declarations,
  TypeScript/Vite build, pinned formatting and 55 relative documentation links
  passed. The dispute UI shows credits and original fiat currency separately;
  both locales describe its current Close action accurately.
- Independent source review found and closed source-guard pool starvation and
  reversed-effect replay gaps, then passed the final source, native callback
  normalization and worker-queue boundary.

After rebasing onto checkout-route main `23b6547`, strict workspace Clippy
(`--all-targets -- -D warnings`), API generation and frontend build passed.
The complete workspace run remains pending before merge. These checks are not browser, real PSP or candidate
acceptance; #725/N10 and the final release gates remain open.
