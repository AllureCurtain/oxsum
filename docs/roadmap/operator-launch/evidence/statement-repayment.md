# Exact statement repayment (#777)

Implementation branch: `feat/statement-repayment`, based on source prerequisite
PR #918 (`2d3d7e99`) over main `5285fc0b`. Migration 0122 follows reserved
0120/0121 from the independent application-read/first-call work.

## Implemented behavior

- Personal-customer read-only partial/full quotes use exact canonical journal
  sources, including chargeback-only and mixed-policy documents. Historical
  duplicated usage remains review-required. Integer debt is pinned independently
  of the ceiling-rounded fiat charge and collection revision.
- Order, immutable snapshot, payment binding and debt reservation commit together.
  Concurrent intents cannot overreserve. Exact retry keeps original terms;
  unknown provider outcomes reserve, authenticated failure/expiry releases.
- Cash requires a matching retained verified success receipt. Cash/AR, exact
  journal/statement lines, collection-only daily effects, reservation, fulfillment
  and outbox commit together. Released late payments become source-linked payable.
- Refund and lost-dispute effects consume original retained payment allocations.
  Refunds restore debt without fabricating dispute counters. Excess confirmed
  external loss becomes suspense; wallet, CreditLine and entitlement are untouched.
- Source-exclusive financial voids cover original and chargeback follow-up
  documents and pin each repayment's payable share to the original effect and
  canonical void entry. Old plan-order returns cannot consume those shares.
  Mixed-source voids retain their explicit review refusal before mutation.
- Debt clearance recomputes only debt dunning. Manual suspension, archive,
  cancellation, service intervals and entitlement remain independent.
- Customer Billing supports partial/full review and pending-order recovery; the
  order page distinguishes repaid debt from late payable. Finance can follow the
  exact statement/journal and read payment/refund/dispute allocations, canonical
  entry IDs/hashes and review amounts under current ReportsRead authority.

## Validation

Workspace coverage was executed once and resumed without repeating unchanged
passes: 751 initial passes, 1,418 remaining passes, then the corrected migration
inventory test. All 2,170 ordinary tests have passing evidence. Six pre-existing
opt-ins (vector/fixture emission, capacity, saturation and real providers) remain
outside this local run. The three earlier failures were old-schema snapshot and
new-table inventory assertions; upgrade tests now require new snapshots to be
null rather than manufacturing historical payment authority.

Focused final coverage includes financial source comparisons, migration upgrades,
statement HTTP, local Hyperswitch-style checkout and direct finance evidence.
Tests cover v1/v2 partial/full and mixed allocations, same/different keyed races,
changed collection terms, live-intent void fences, atomic rollback after cash,
late/repeated/refund/lost callbacks, source-owned void payable, absent terminal
history, one-connection provider-only correlation, cancelled-plan debt and actual
wallet admission with independent manual suspension. HTTP checks reject foreign,
machine, missing-session, stale-quote and under/over-fiat requests before provider
I/O; exact create replay issues one local checkout.

Strict core/server all-target Clippy, pinned formatting, four OpenAPI security
checks, generated TypeScript and the frontend build passed. Fifteen money and
retained-evidence/frontend grant tests passed. The pre-existing bundle-size
warning remains. Final receipt-source regression: 41 source tests passed,
including rejection of a paid status with no authenticated cash receipt. This
adds one case beyond the 2,170-case workspace run; unchanged passes are reused.

## Remaining acceptance

This is software/fixture evidence. Independent review, protected integration,
actual browser acceptance, genuine PSP and final candidate gates remain pending.
The prior browser task space disappeared; no replacement/user space was used.
No real provider attempts or payment spend occurred (run-wide 0/5, CNY 0).

Mixed-source repayment and returns are implemented. Mixed-source *financial
void/reissue* remains an explicit safe refusal and needs a source-specific
operator resolution before #777 can claim complete unrestricted recovery.
The issue stays open; the PR must not silently narrow that acceptance.

Follow-up #927 implements source-specific journal voids for verified mixed
documents; see [statement-source-void.md](statement-source-void.md). The original
PR's validation above is retained as historical evidence. Whole-statement void
still refuses mixed sources; replacement invoicing remains separate.
