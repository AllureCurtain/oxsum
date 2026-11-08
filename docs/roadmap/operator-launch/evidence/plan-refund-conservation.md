# Plan refund and settled-usage conservation evidence

Issue: [#779](https://github.com/AllureCurtain/oxsum/issues/779) (child of N10/N11 under #736/#737/#768/#725). Branch `fix/plan-refund-conservation`, based on main `0314cbd`. Development evidence, not a merged candidate; no real provider, model or payment was driven.

Six hypotheses from the source audit were each driven through a real-ledger fixture (`oxsum_testkit` scratch database, a real `Wallet` on `Tenants`, `fulfilled plan_period` order rows standing in for the PSP half). Every fixture runs open → action → close/settle and asserts the conserved figure: the receivable, the recognized income, the credit note's terminal state and the customer statement's `total_minor`/`paid_minor`/state.

Suite: `crates/core/tests/plan_refund_conservation.rs`.

Current correction: [#802 cash allocation evidence](plan-refund-cash.md) supersedes the H2/H3 deferral and the old H3 cash split below. Confirmed refunds return the full external amount; the historical text is retained as the original failed hypothesis record. H2/H3 reproductions are active after #802.

## Verdicts

### H1 — wallet-settled usage re-billed as a plan receivable — CONFIRMED, deferred to N11

`journal_closing_period_in` (`journals.rs`) sums the period's settled `charged_minor` from `usage_records` into a `usage_overage` line, and the accrual books `debit Assets:Receivables / credit Income:PlanFees` over every positive charge leg — including that overflow. Fixture: overage-enabled metered plan, 3 credits of overflow settled inside period 0 → the close wrote `usage_overage=3_000_000`, `total=3_002_500`, `planFees=3_002_500`, `receivable=-3_000_000`. The usage was already collected through the wallet at request time (`admit_subscription_draw` holds `minor − drawn` against the wallet pools); billing it again on the invoice asks the customer to pay the same usage twice.

Deferred because whether overage bills on the invoice at all is the revenue-recognition decision the issue assigns to N11's versioned daily recognition — the existing `close_bills_overage_on_an_overage_enabled_plan` test and `docs/product.md` encode `usage_overage` as the intended overage design, so removing the charge leg is a product-semantics change, not a mechanical fix. The fixture is `#[ignore]`'d and reproduces the defect on `--ignored` for the N11 work.

### H2 — a mid-period plan-fee refund's note is never consumed — CONFIRMED, deferred to N11

`settle_order_refund` routes a `plan_period` refund through `apply_refund_to_journal_in`, which returns `None` when no journal exists yet — so the note stays `issued` and no `Discounts→Cash|Receivables` leg is booked. The close's own credit-note scan then reads `state='applied'` and skips it. Fixture: refund confirmed mid-period → `apply_refund_to_journal` returned `None`, note stayed `issued`, close produced `total=2500` with only `[(BaseFee,2500)]` — no discount line, note unconsumed, refund's money never surfaced.

Deferred because consuming the note correctly is the deferred/earned allocation the issue assigns to N11: reducing the receivable and returning the paid-order cash are two different corrections that current legs do not separate. `#[ignore]`'d reproduction.

### H3 — a refund past the open receivable books credit, not cash — CONFIRMED, deferred to N11

`apply_refund_to_journal_in` caps the credit at `journal.total_minor` and, on a `posted` (partially-paid) journal, books `debit Discounts / credit Receivables` regardless of how much is still owed. Fixture: 1500 open receivable, refund 2000 → `receivable` went `-1500 → +500` (a negative net receivable the customer never owed), `cash` unchanged. The 500 beyond the debt should have returned as cash, not erased a receivable that did not exist.

Deferred for the same N11 reason — the split between open receivable and collected cash is the deferred/earned allocation. `#[ignore]`'d reproduction.

### H4 — the statement's charge total excludes payment lines, but `paid_minor` missed the prepay — CONFIRMED, FIXED

`issue_statement_for_journal_in` summed only the journal's charge lines into `total_minor` (correct — payment lines do not inflate the charge), but inserted `paid_minor` defaulting to 0 while the close's prepay had already reclassified part of the receivable. A partially-prepaid journal (`settled_minor=1000`, `posted`) issued a statement with `paid_minor=0`, so a later payment reconciled from the wrong base and the statement stayed `issued` while its journal was `paid` — the customer-facing document under-reported what was already covered. Fixture before fix: `issued_total=2500`, after a 2500 renewal `paid_minor=1500`, `state=issued` though the receivable was fully settled.

Fix: `issue_statement_for_journal_in` seeds `paid_minor` from the journal's `settled_minor` (clamped to `total`) inside the same commit, so the statement opens carrying the share the prepay already covered. After fix: `paid_minor=2500`, `state=paid`. Verified non-regressing — `a_partial_payment_leaves_the_statement_issued` and the rest of the journals/plan_statements/plans/refunds suites pass.

### H5 — a lost-dispute chargeback lands on a different open statement — DISPROVED (designed fallback)

`chargeback_statement_in` appends the `disputeChargeback` onto the next open statement, earliest-due first — not the statement wrapping the contested journal. That is the designed surface, not a defect: a lost dispute only re-opens a `paid` journal, and the `plan_statements` state machine (`migration 0095`) permits `paid → void` only — a `paid` statement cannot be reopened, so the restored receivable must land on another document. Fixture: contested `paid` journal's statement got `chargeback=0` and stayed settled; the open period-1 statement carried `chargeback=2500` exactly once. Conservation holds — the debt is never invisible nor doubled. The original selection is correct once H4 keeps `paid` statements genuinely closed.

### H6 — voiding a journal does not synchronize its customer statement — CONFIRMED, FIXED

`void_invoice_journal` reversed the journal's lines and posted the ledger reversal but never touched `plan_statements` — the wrapped statement stayed `paid`/`total=2500`/`paid=2500` after its receivable was voided, the customer-facing document outliving the bill.

Fix: new `void_statement_for_journal_in` in `plan_statements.rs`, called inside `void_invoice_journal`'s transaction, appends reversing lines and walks the statement to `void` (permitted `paid → void`/`issued → void`). After fix the statement reads `Void`/`total=0`. Verified non-regressing.

## Validation

`cargo nextest run -p oxsum-core --test plan_refund_conservation` — 3 active tests pass (H4, H5-designed, H6); 3 confirmed reproductions run under `--ignored` (H1, H2, H3). `cargo nextest run -p oxsum-core --test journals --test plan_statements --test refunds --test plans` — 35 passed, 0 failed. `scripts/fmt.sh` clean; `cargo clippy -p oxsum-core --all-targets` clean.

## Remaining gates

H1, H2 and H3 need the N11 versioned daily-recognition policy to land the deferred/earned allocation they each isolate; the `#[ignore]`'d reproductions are ready to un-ignore and pass once that decision is implemented. The legacy `usage_overage`, mid-period-note and over-receivable policies stay explicit in the interim. No real model, provider or payment spend was attempted.
