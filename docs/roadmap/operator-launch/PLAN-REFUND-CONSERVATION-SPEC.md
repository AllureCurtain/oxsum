# GUARDRAIL SPEC — #779 Plan refund & settled-usage conservation across journals

Work unit: child of N10/N11 under #736/#737/#768/#725. Branch `fix/plan-refund-conservation`, base `main` @ `0314cbd`. Issue: https://github.com/AllureCurtain/oxsum/issues/779

## Mission

A source audit flagged correctness risks where plan refunds, disputes, voids and already-settled usage interact with invoice journals and customer plan statements. **Every item below is a hypothesis, not a confirmed defect.** The acceptance contract requires each applicable defect be *reproduced with exact real-ledger fixtures* — opening / action / closing ledger legs plus the customer document totals — before any behavior changes. Do not rewrite history: corrections are explicit, source-linked, idempotent and auditable.

## Method (required order)

For each numbered hypothesis:

1. Write a focused PostgreSQL fixture (real `Db`, `oxsum_testkit::suite_db_url`, real ledger `Wallet`) that drives the scenario end to end: open → the trigger action → close/settle. Assert the *conserved* quantity (cash, receivable, income, the customer statement's `total_minor`/`paid_minor`, the note's `state`).
2. Run it. If the conservation invariant holds, the hypothesis is **disproved** — record that in the evidence file and move on; do not "fix" a non-defect.
3. Only when a fixture reproduces a real violation, design a minimal correction: a new ledger entry / journal line / statement line that is **linked** to its source (`source_ref`, `replaces_journal_id`, refund/dispute id), **idempotent** (replay is a no-op), and leaves existing postings and issued documents untouched.

## Hypotheses (with the code that motivated each)

H1 — **Settled usage billed twice.** `crates/core/src/journals.rs:766` (`journal_closing_period_in`) sums `usage_records.charged_minor` for `settled_at ∈ [period_start, period_end)` into a `usage_overage` line. Those turns were *already* settled to wallet/credit at request time; adding them again makes a second plan receivable/income for the same usage. Fixture: overage-enabled plan, metered calls inside a period, close → journal must not carry a `usage_overage` line duplicating what the wallet already paid.

H2 — **Pre-close refund leaves the issued note unconsumed.** A refund confirmed before the closing journal exists is picked up by the close's own credit-note scan (`journals.rs:799`), but the note's `state` may stay `issued` rather than `applied`. Fixture: refund resolved mid-period → close → note must be `applied` exactly once, journal carries the `discount` line.

H3 — **Refund on a partly-paid journal moves receivable, not cash.** `apply_refund_to_journal_in` (`journals.rs:1113`) reduces `total_minor`/`settled_minor` and books `plan_journal_refund_in`; on a `posted` (partially settled) journal the returned money must come out of *cash*, not just shrink the receivable the customer still owes. Fixture: partial payment → refund → assert cash decreases and the open receivable reflects what is genuinely still owed.

H4 — **Statement gross total includes payment lines.** `plan_statements.rs:343` sums `lines_for_statement_in` (journal charge lines) into `total_minor`; `payment_applied`/`dispute_chargeback` are appended later. Verify `total_minor` is a *charge* total and payment lines only move `paid_minor` — confirm whether any read/validation conflates them. Likely disproved; the fixture proves it.

H5 — **Dispute/repayment maps to the wrong open statement.** `recharge_journal_dispute_in` (`journals.rs:1232`) targets the order's own `paid` journal; `chargeback_statement_in` appends onto "the next open statement". When several open statements stand, the chargeback must land on the statement that wraps *this* journal, not an arbitrary open one.

H6 — **Paid-journal void does not synchronize the customer statement.** `void_invoice_journal` (`journals.rs:491`) reverses journal lines and posts the mirroring entry; check the wrapped `plan_statements` row follows (void/restated) rather than dangling `paid`/`issued` after its journal is void.

## Guardrails

- Money is integer minor units; never float. SCALE = 1_000_000.
- Ledger table structure is doubleentry-managed; never hand-edit. oxsum tables only via new migrations (`crates/core/migrations/0099_*.sql` next number — confirm the highest existing).
- Preserve existing postings and issued documents. No `UPDATE` that rewrites a committed financial fact; corrections are new linked rows/entries.
- No real model/payment spend; local fixtures only. Do not describe local signed events as PSP certification.
- English only: code, comments, commit messages, docs.
- rustfmt via `scripts/fmt.sh`; `cargo clippy --workspace --all-targets` must be clean; `cargo nextest run` on the touched suites.
- Interface changes go through `crates/server/openapi.yaml` first. Internal-only corrections need no contract change.
- If a hypothesis requires N11 (versioned daily recognition) semantics that do not exist yet, record the dependency and keep the legacy policy explicit — do not half-implement N11 here.

## Deliverables

- One focused test file (e.g. `crates/core/tests/plan_refund_conservation.rs`) with a real-ledger fixture per hypothesis.
- Minimal corrections for the *confirmed* defects only, each source-linked and idempotent.
- `docs/roadmap/operator-launch/evidence/plan-refund-conservation.md`: per-hypothesis verdict (confirmed/disproved), the reproduction, the correction, and validation counts.
- `TODO.md` entry. Commit split: reproduce-fixtures / correction / docs+evidence.
