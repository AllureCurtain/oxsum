# Atomic monthly recorded payments (#1013)

Base main: `2ee706019a02f24b4b8ed4b949b8c5254b27a178`.
Prerequisites: #1010 atomic issuance/borrowed ledger reads and #975 historical
bundle-refund fixture repair. Both are included as separate merge commits.

## Reproduced baseline

Run `statements::a_statement_generates_finalizes_and_is_paid` with the new paid
replay assertion and the prerequisite's original production files. It failed:
expected HTTP 200, received HTTP 400 `the statement is paid; nothing is owed`.
The production files were restored immediately after the controlled baseline.
The repaired endpoint passes that same assertion.

The old route also checked period debt before a separate `top_up`, so a queued
repayment could use obsolete debt and fund wallet excess. The new core method
serializes the period cap with the credit-only posting, instead of splitting a
payment into wallet funds. Source audit, standing and events share its commit.

## Financial and replay evidence

Focused PostgreSQL checks cover:

- Earlier unbilled draws consume repayment before the selected statement.
- Paid exact replay retains the original entry; changed amount/statement and a
  new payment after full repayment refuse.
- Two concurrent exact requests converge on one entry; distinct full payments
  cannot both book. Later-period debt cannot raise the selected period's cap.
- Concurrent top-up/repayment conserves debt and permits only the independent
  top-up's excess to enter wallet funds. A controlled credit lock queues top-up
  first and proves the repayment recomputes the period cap after waiting.
- Foreign wallet/customer identity and globally reused audit key refuse.
- Legacy credit-only cash plus matching original audit replays; cash without
  its source audit is refused without another posting.
- Audit and notification failure each roll back cash, standing and operation
  identity. The exact retry succeeds once; its notification is not duplicated.
- Both fresh posting and paid replay execute with one pooled connection.

Initial related coverage passed all 46 selected core/HTTP/credit/order cases,
including PDF, signed/unsigned proofs and lifecycle behavior. Complete workspace run `28aedb06-469d-4f4d-939b-8b9d3bfd6c76` on
`ca026a12` executed **2,266 tests, all passed**, with six existing opt-ins
skipped. After adding two further fault/controlled-concurrency regressions,
final focused run `543bb35e-f9bd-4979-8b30-c300111f7713` passed all **7/7**
monthly payment cases. Production code did not change after the full run.
Strict all-target Clippy for core/server, pinned formatting, five contract tests,
OpenAPI code generation and diff checks passed.

No migration, provider call, payment service call or browser was needed for this
backend correction. Staff portal payment-reference collection, browser/customer
acceptance (#1006), independent review, protected integration and exact merged
candidate gates remain distinct obligations.
