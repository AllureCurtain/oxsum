# Non-plan refund Cash and exact historical recovery (#841)

This child of #829 covers wallet top-ups, bundles and legacy CreditLine repayment
refunds. Historical wrong-direction chargebacks remain #842 and scoped statement
repayment remains #777. No live correction, PSP call or production deployment was
performed. Browser journeys and final candidate acceptance remain open.

## Implemented boundary

- An exact retained authenticated refund success records full `Cr Cash` once.
  Proven retained original Wallet supplies the debit; spent, held, repayment
  origin and uncertain funding use `Equity:Adjustments` review suspense. Later
  credits cannot replenish the original source, and debt/quota is unchanged.
- An unapproved or terminal late cash fact remains distinct from approval state.
  A requested or declined refund may retain actual Cash with no credit note or
  invented human actor. Reversed notes and historical entry pointers stay intact.
  Effects consume credit and fiat bounds, fence approval/provider claims and
  fence any unperformed purchase grant, including after a partial cash return.
- Immutable migration 0115 effects bind source/receipt, Cash/correction entries,
  hashes and original Cash booking date. Reports count Cash once on that day;
  noncash restitution is excluded. Customers and finance see complete allocation,
  review reasons, source identity and restored Wallet independently of state.
- Exact local preview/apply requires current personal Business `fundsWrite`,
  second-factor checks, explicit refund/receipt and unchanged source revision.
  The CLI requires an explicit database URL and existing wallet, does not discover
  dotenv, migrate or contact a PSP, and defaults to a write-free preview.
- Retained old `Dr Wallet / Cr Cash` is verified at its exclusive historical
  sequence boundary. Only proven excess Wallet debit is restored through
  Adjustments/Wallet, preserving old Cash identity/date/hash and note pointer.
  Missing, reversed, incompatible or ambiguous historical evidence refuses.
- The original, later activity and actual return entries are verified through
  #856's canonical borrowed-connection reader. The 20,000-entry review cap places
  new Cash in suspense and refuses historical restitution; it does not prove
  spent source. Stop/drain older writers before the new versioned policy starts.
- Parent rows precede source/refund/append locks. Cash, promo cancellation, effect,
  applicable note and audit commit atomically. Single-connection callback,
  recovery and replay are covered. This does not certify every financial path
  for a single connection.

## Regression evidence

All runs use the isolated worktree's empty ignored `.env` and explicit loopback
`DATABASE_URL` plus `DOUBLEENTRY_DATABASE_URL`. Neither provider credentials nor
real customer data are loaded.

- Initial combined run: 80 of 81 checks passed; the existing concurrent refund
  lease test exposed the new source try-lock returning `PoolBusy`. The corrected
  worker claim returns no lease, preserving retry semantics. A 34-check refund
  rerun passed (`60ccc978-e8cf-4d23-8cdd-f4ea6a88fb79`, 7.291 seconds).
- Main CI [42525666821](https://github.com/AllureCurtain/oxsum/actions/runs/42525666821)
  and [42526621594](https://github.com/AllureCurtain/oxsum/actions/runs/42526621594)
  independently failed the first refund request immediately after fulfillment.
  SQLx's dropped source transaction queued rollback after success was returned.
  All three successful fulfillment return paths now await source release.
- The controlled regression pauses queued Drop cleanup with a pool release
  hook, then uses a separate pool to claim the source lock immediately after
  success. Original code fails; the fix passes initial fulfillment and replay
  (`59644139-4203-4007-80f3-d3e821289563`). It does not add scheduling sleeps.
- Final focused core run passed all 69 checks, zero skipped, covering 25 new
  non-plan scenarios, existing refunds, plan refunds/conservation and payment
  orders (`bbc67e1f-bf2c-4ebf-a661-0d70f894ee42`, 18.974 seconds).
- All 40 HTTP refund/payment/merchant-event/financial-facts checks passed, zero
  skipped. They include complete spent-source Cash, customer/note projections,
  signed identity and timeout recovery.
- The delivered CLI's two argument checks passed. An isolated executable fixture
  invoked the actual built CLI for eight passing scenarios: pure default preview,
  stale source refusal, pinned apply and exact replay, unauthorized session,
  ignored cwd dotenv/missing explicit URL, changed same-key reason, unknown
  receipt and unpinned apply refusal before database access. Temporary fixture
  code was removed after execution; the delivered command is unchanged.
- Four API contract checks, TypeScript generation and production frontend build
  passed. Independent source review passed the canonical source/receipt/history,
  parent lock hierarchy, report clock, CLI, lease contention and awaited source
  release changes.

Complete workspace and strict validation evidence is recorded below once run.
