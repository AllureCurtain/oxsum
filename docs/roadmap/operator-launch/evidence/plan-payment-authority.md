# Plan payment authority development evidence

Issue: [#776](https://github.com/AllureCurtain/oxsum/issues/776). This branch depends on collection authority [#771](https://github.com/AllureCurtain/oxsum/pull/771), based on main `0314cbd`. It is development evidence, not a merged candidate or real PSP certification.

## Implemented behavior

The customer's plan payment quote pins the assignment, sold version, zero-based period, exact credit fee and business collection revision/rate/currency. A deterministic fingerprint binds checkout to those terms. Fiat minor units use checked integer ceiling; the entitlement fee is never computed back from the rounded fiat amount. Customers cannot choose a promotional grant, another customer's plan or a cheaper payment for the same entitlement.

One live order reserves a plan/index. Unknown and attention states retain it; retrying the original request or reading its standing quote recovers that order. A first period starts at paid activation. Manual renewal becomes payable only at the current UTC boundary and pins the next boundary. It cannot truncate a paid active period. Cancel/change waits for unresolved unactivated orders; already-committed authorization can finish its outer fulfillment after a crash, including a subsequent refund reservation.

Activation independently verifies actual paid state, paid time, purpose, customer and immutable terms before any entitlement or ledger effect. Exactly the sold fee prepays its own period. Outstanding statements remain separate; explicit partial/full repayment is [#777](https://github.com/AllureCurtain/oxsum/issues/777). The customer portal exposes review, payment and recovery without hand-entered identifiers.

Migration 0103 retains legacy completed authorizations, rejects old-writer inserts without exact snapshots and never invents terms for pending legacy orders. Unsafe historical orders remain visible to finance but cannot grant a new period. A matching historical completed authorization may replay without writing another period.

## Validation

The tests use PostgreSQL 17 with explicit `DATABASE_URL`; the full run also sets `DOUBLEENTRY_DATABASE_URL`. A short isolated local provider stub and signed fixture event exercise HTTP checkout/callback/recovery. No root `.env` credential is copied into the worktree; its ignored `.env` is empty.

- Core plan/journal/statement/payment boundary selection: 49 passed, seven binaries, 9.881 seconds.
- Extended core/server quote, migration, refund and old-fixture compatibility selection: 46 passed, 9.619 seconds.
- HTTP payment suite: 15 passed, 3.814 seconds. Forged amount, target and fingerprint cause zero checkout calls. Replayed valid payment causes one checkout; a second live intent conflicts; signed payment and duplicate callback yield exactly one period and zero wallet balance. Early renewal conflicts.
- Strict workspace Clippy passed in 2m44s and again after the recovery-queue and statement-line additions.
- Frontend typecheck/build, formatting and diff validation passed. Actual browser acceptance remains pending the task browser's recovery.
- Final gates on this tree: focused plan/statement/order/refund suites plus the HTTP payment suite, 83 passed; upgrade/fault suites (collection, plan-payment upgrade, pool exhaustion, concurrency) 19 passed; strict Clippy 4m22s clean. The earlier full-workspace run surfaced a stale `to_jsonb` snapshot in `collection_upgrade` (the new nullable `plan_payment` column) and a load-dependent pool-exhaustion flake; the snapshot assertion was corrected to ignore the new column and the suites rerun green.

## Remaining gates

Combine with bundle [#778](https://github.com/AllureCurtain/oxsum/pull/778), collection and payment recovery PRs, then run the final integrated gates on the merged SHA and actual artifacts. Genuine review approval and actual browser acceptance are outstanding. Plan refund/usage/journal correction audit is [#779](https://github.com/AllureCurtain/oxsum/issues/779); versioned daily recognition is N11/#737. Neither is claimed complete by this authority repair. No live provider inference, credential authentication or real payment was attempted; shared budget remains 0/5 attempts and CNY 0.
