# Statement review and debt conservation (#820)

## Scope and outcome

Statement review previously zeroed only its customer document, leaving the
journal receivable outside customer credit/admission. A grace extension could
also clear the plan-wide debt flag while another bill remained past grace.

The repair shares one transaction-level journal void across staff journal and
statement actions. Proven original journal/statement sources reverse together,
Cash remains unchanged, collected funds become RefundPayable, and the audit
records the actual reversal entry. Ambiguous chargeback/reissued/mismatched
sources refuse before effects. Exact target/action/reason/key replay is inert.

A shared debt projection includes live statements and exact posted journals
without a live document. Retained voids use their retained due date; missing
documents fall back to the original binding's current configured net terms
plus posting time. Customer credit and overage limits retain those debts.
Wallet admission checks all effective past-grace debt, including ordinary live
statements, before the periodic worker stamps a flag. Grace, payment, historical
usage correction and financial void refresh only debt suspension; manual,
archive, cancellation, renewal and entitlement state remain independent.

## Controlled baseline

Using production sources from `1ebb7e4` with the new unchanged test cases:

```bash
cargo nextest run -p oxsum-core --test statement_review_finance \
  -E 'test(statement_void_reverses_paid_and_partial) | test(grace_extension_preserves_other) | test(retained_void_debt_counts)' \
  --no-fail-fast
```

All three selected cases fail at the intended assertions:

- Statement void leaves its journal `Paid`, rather than `Void`.
- Extending one overdue bill clears the dunning marker while another remains.
- A retained document-only void is excluded from credit-limit admission.

Six unrelated cases were filter-skipped in this baseline comparison. Production
sources were restored before validation; this is not a passing-suite claim.

## Development verification

Before the historical usage repair rebase, the combined core/server command
below passed all 57 cases with zero skips; strict workspace/all-target Clippy
with `-D warnings` passed. OpenAPI security checks (four), generated types and
the production frontend build passed.

```bash
cargo nextest run -p oxsum-core -p oxsum-server \
  --test statement_review_finance --test plan_statements \
  --test plan_statement_locks --test journals --test plan_refunds
```

Coverage includes paid/partially paid/zero/unpaid-after-refund voids; actual Cash,
Receivables, PlanFees and RefundPayable postings; two overdue debts; real wallet
admission; manual suspension; retained document-only void credit cap and due
date; foreign wallet/mixed-chargeback/malformed reversal refusal; exact and
concurrent replay; HTTP authority/errors; and statement-write failure after
ledger append rolling back journal, money and audit together. Existing #811
controlled lock/race assertions remain active.

After rebasing onto `68b3441`, runtime/test snapshot `a478413` passed the complete
workspace: **1,937 passed, zero failed, six existing opt-in skips** in 248.738
seconds. All #809 and #820 cases ran, including both journal/statement void after
correction and correction of one bill while another retained void still
justifies dunning. The skips are two fixture-regeneration helpers, two real
provider integrations, the load check and reference capacity; none is a skipped
financial hypothesis.

A subsequent narrow restoration change lets a new explicit review against an
already-financially-void journal recompute a stale debt marker and audit that
review with no reversal entry. Exact audited retries remain inert. This final
change is validated by the focused checks recorded below rather than attributed
to the earlier complete run. The final combined core/HTTP selection passed
**66 of 66**, zero skips, in 17.220 seconds:

```bash
cargo nextest run -p oxsum-core -p oxsum-server \
  --test statement_review_finance --test plan_usage_corrections \
  --test plan_refunds --test plan_statements --test journals
```

Final runtime/test commit `681eed9` also passed `scripts/fmt-check.sh`, strict
workspace/all-target Clippy with `-D warnings` (71 seconds), the four OpenAPI
security checks, type regeneration and the production frontend build.

The final ten statement-finance scenarios include both already-void entrypoints,
no extra ledger effect, exact-retry inertia and preserved manual restriction.
The existing lock cases passed in the full run. English and existing Chinese
confirmation text describe the same financial behavior.

## Limits

These are local PostgreSQL, ledger and production-router checks. No PSP call,
provider use, cash payout, historical production repair or public deployment
was performed. New explicit statement repayment remains #777, dispute terminal
atomicity remains #826, and daily recognition remains N11. Candidate, browser
journey and exact released-image acceptance remain separate gates.
