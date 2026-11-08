# Closing plan prepayments apply to customer statements once

Issue: [#795](https://github.com/AllureCurtain/oxsum/issues/795), following the
integration of #776 and #779 on main `c79a588`. This is development evidence,
not a candidate acceptance or provider certification.

## Integration defect and correction

The #779 statement issuer prefilled `paid_minor` from its journal. The #776
closing path then applied the same prepayment through the allocation writer.
Full prepayment left no outstanding amount for that writer, so it returned
before adding the payment line or advancing `issued` to `paid`. Partial
prepayment was added twice, understating the outstanding receivable.

The issuer now starts the statement with zero allocated payment. The existing
closing caller is the only call site of the issuer and allocation writer: it
applies `journal.settled_minor` once, after issuance, in the same transaction.
That writer appends the order-linked payment line, advances `paid_minor`, and
marks full coverage `paid`. No statement with temporary zero coverage becomes
visible before this transaction commits. Gross charge totals still exclude
payment lines. No API, schema, posted ledger entry or historical document is
rewritten.

The exact-paid-period regression checks the statement's paid state, original
order, one payment line and agreement with the journal. Repeated rollover and
activation preserve that result. The partial case verifies a 3,500-minor
legacy bill against its 2,500-minor prepayment: one -2,500 payment line, a
1,000-minor remainder, and no debt repayment from a subsequent renewal.

## Conservation fixture compatibility

All plan orders in `plan_refund_conservation` now use the shared production
quote/order and local verified-event fixture. Direct inserts of unquoted
`plan_period` orders are rejected by migration 0109; underpaying a plan fee
and diverting a renewal to old debt are no longer valid fixture operations.

H4 uses exact plan funding plus the explicitly retained legacy overage policy
to expose partial coverage; it asserts the old statement remains unchanged
across renewal and replay. H5 uses a second exact-funded period with legacy
overage to retain an open statement, then verifies the original chargeback
lands once while the paid statement stays closed. H6 drives a genuinely paid
journal through the staff void and checks its statement follows.

H1, H2 and H3 remain explicitly ignored, unresolved N11 reproductions. Their
current order setup is compatible with the new authority; this change neither
endorses legacy overage billing nor certifies the refund allocation policy.
The H3 setup now creates its partial journal through retained overage rather
than an invalid plan underpayment. The N11 correction must validate its actual
confirmed-cash-refund semantics, not merely change code to fit an old fixture.

## Validation

- With the main `c79a588` implementation temporarily restored, the two
  statement regressions both fail: full coverage remains `issued`; partial
  coverage incorrectly becomes `paid`. After restoring the correction both
  pass. The strengthened test assertions were unchanged between runs.
- `DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres cargo
  nextest run -p oxsum-core --test plan_statements --test
  plan_refund_conservation --test journals --test plans --test plan_payments
  --test payment_orders --test refunds --no-fail-fast`: **55 passed**, three
  explicitly ignored N11 cases, 10.741 seconds. Scratch databases were created
  on the PostgreSQL 17 development cluster; the active H4/H5/H6 fixtures all
  executed and passed.
- `scripts/fmt-check.sh`, `git diff --check` and
  `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- Local ignored logs: `local-notes/statement-prepayment-before.log`,
  `statement-prepayment-after.log`, `statement-prepayment-nextest.log`, and
  `statement-prepayment-clippy.log` in this task's worktree.

The final combined main suite and candidate acceptance remain separate gates.
No live model, PSP, external email or money was used.
