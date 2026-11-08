# Lazy cancellation admission (#854)

Status: contract, test-only baseline and implementation prepared. No runtime
validation is claimed yet; final integration follows funded/free admission #849.

The lazy plan rollover previously returned `false` for both a cancelled plan and
a still-serving past-due window. Admission proceeded to draw the ended quota in
both cases, so the first request after a cancelled boundary could consume it
before the worker ran. Cancellation also left quota-period history marked open.

The planned regression exercises both overage policies without running the
worker first, requires no new draw or entitlement, then settles a pre-boundary
reservation twice and checks the retained period and once-only invoice close.
The production repair rechecks subscription state under the existing lifecycle
lock and closes quota history with cancellation. The past-due remainder path
remains active. The #849 borrowed admission API will receive the same state
check after rebase, plus gateway refusal rollback and concurrent boundary checks.

Test-only baseline commit: `600c7dc1`, production unchanged from `f3802d5e`.
Execution results and exact final code revision will be added before PR review.
