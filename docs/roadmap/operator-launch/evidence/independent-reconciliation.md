# Independent financial reconciliation (#848)

Status: development validation and independent source review passed; protected
PR integration pending. N11/J12 browser conservation and exact-candidate
acceptance remain open.

## What changed

The previous writer cloned `financial_facts` into both expected and actual
totals. The new capture reads immutable business sources and all bounded tenant
ledger entries in one repeatable-read transaction. Canonical entry bytes are
verified through the shared borrowed reader, with raw key, deterministic entry
identity, sequence position, registered account and source ownership checked
separately. Actual amounts come only from verified settled USD postings.

The compared fields are billed, earned, deferred, collected, receivable and
confirmed refund Cash. V1 uses the original-posting journal cohort with retained
net corrections. V2 uses signed movements under booking, service or payment
attribution. Payable and suspense remain distinct evidence, not revenue or debt.
Usage, entitlement, funding, discount, provider cost, legacy deferred balances
and actual service delivery are explicitly excluded from the reported totals.

The run is a latest pointer; every completed attempt retains its original source
rows, exact ledger evidence, differences and coverage. Exact keyed manual retries
return their original attempt. The dashboard can select old attempts independently
of the current exception work list. Migration 0117 preserves old projection-only
reports with coverage version 0 and `legacy` state. It follows integrated funded
admission migration 0116.

## Observed source and action checks

All values below are integer minor units. These are controlled PostgreSQL/ledger
fixtures; they do not certify external payment providers or service delivery.

| Source/action | Billed | Earned + deferred | Collected | Receivable | Refund Cash |
| --- | ---: | ---: | ---: | ---: | ---: |
| Original v1 or v2 paid fee | 2,500,000 | 2,500,000 | 2,500,000 | 0 | 0 |
| Original-source 1,000,000 refund | 1,500,000 | 1,500,000 | 1,500,000 | 0 | 1,000,000 |
| Later lost original payment | 1,500,000 | 1,500,000 | 0 | 1,500,000 | 1,000,000 |
| Financial void, then refund and excess loss | 0 | 0 | 0 | 0 | 1,000,000 |

The lost payment preserves earned/deferred income, restores only remaining
original collection as receivable and retains excess cash loss in suspense.
The void reverses revenue/deferred and retains collected funds as refund payable.
Its immutable header keeps the historical gross amount while reversal lines
net to zero. The first focused run exposed this distinction in the comparison;
both affected void regressions passed after correcting the expected line total.

The 22 new internal tests also cover internally balanced effect/header drift,
same-total payment-source substitution, raw-key mismatch, corrupt posting hash,
missing accounting effect, renamed account path, mixed-policy chargebacks,
late refund after an exhausted original payment, original refund receipt
identity, current authority, repeatable-read consistency, exact replay, immutable
history, one-connection capture, nonblocking contention and publication rollback.
Three additional historical refund/usage-correction integration cases prove
original cash/restitution and retained correction digest/entry checks.

Fees of 3 and 17 minor units exercise zero-money recognition days. Sequenced
neighbors and retained correction boundaries establish their order where
possible. A controlled refund with an earlier booking date and transaction start
across UTC midnight retains valid monetary entries but lacks proof of the zero
day's exact order. It therefore reports incomplete coverage and excludes that
period's schedule rather than claiming a verified allocation. Equal transaction
timestamps do not collapse distinct official movements. Free-plan activation
and void do not invent monetary entries.

## Executed validation

Explicit local PostgreSQL URLs were supplied, so database suites executed rather
than skipping for an absent environment.

| Gate | Result |
| --- | --- |
| New `financial_reconciliation::tests` selection | 22 passed: 20 initially, then the two corrected void cases in a focused rerun; all 22 subsequently passed in the complete run |
| Core `financial_facts`, `reconciliation_upgrade`, `plan_usage_corrections`, `nonplan_refunds` | 49 passed, zero skipped |
| Server `financial_facts` | Seven passed, zero skipped |
| `cargo nextest run --workspace` on `773d5bb6` | 2,098 passed, six existing opt-ins skipped, 338.042 seconds of test execution |
| `cargo clippy -p oxsum-core -p oxsum-server --all-targets -- -D warnings` | Passed after equivalent lint cleanup, 4m33s |
| Frontend contract, generated schema and production build | Four contract groups and build passed; current generated schema retained |
| Pinned formatting, diff check, independent source review | Passed |

Local logs: `/tmp/oxsum-848-focused-lib.log`,
`/tmp/oxsum-848-void-regression.log`, `/tmp/oxsum-848-focused-core.log`,
`/tmp/oxsum-848-focused-server.log`, `/tmp/oxsum-848-workspace.log`,
`/tmp/oxsum-848-clippy.log`, `/tmp/oxsum-848-final-frontend.log`.

The complete run included the reviewed #849 final source and #891 publication
repair. Its subsequent lint cleanup only replaced redundant source lookups,
collapsed conditions, made an impossible missing journal a returned error and
documented fixed-inventory/test-fixture lint exceptions. Independent review
confirmed that delta. Rebase onto integrated main `397d07b0` removed the temporary
#849 commit stack; `crates/` and `web/src/` were identical to validated lint head
`53f63409`. Per the focused-validation policy, neither that equivalent-source
rebase nor documentation updates repeat the complete run.

## Boundaries

Capture refuses more than 1,000 customers, 10,000 source rows, 20,000 ledger
entries, 16 MiB of inputs or evidence, or a date span beyond 366 days. It never
truncates a successful attempt. Unsupported or absent sources retain concrete
exceptions; there is no money repair in this reader. Account labels and original
business relationships remain operator-held trust inputs: a content hash alone
does not authenticate them or establish independent log inclusion. A clean
comparison certifies only the declared captured financial-source scope.

No candidate was frozen, no provider/PSP call or publication was performed, and
no browser/customer journey is claimed by this implementation evidence.
