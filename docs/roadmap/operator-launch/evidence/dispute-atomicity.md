# Dispute resolution atomicity (#826)

Base: `68b3441` (after historical correction PR #821). This unit repairs the
three-commit dispute outcome boundary, not the separate non-plan cash policy
tracked by #829 or the statement repayment feature #777.

Implemented evidence:

- One transaction retains business/session authority, owner/dispute locks,
  ledger/document effects, terminal outcome and finance audit.
- Same-key requests compare actor, target, outcome and trimmed reason; different
  terminal outcomes conflict. Same terminal outcome under an unused key is inert.
- Exact customer-wallet ownership and retained sequenced source/audit consistency
  are checked before every replay. Interrupted historical effects require review.
- Seven controlled core tests cover actual PostgreSQL blocker chains, owner-before-
  dispute ordering, after-append audit failure and whole-state rollback, key terms,
  authority, old middle-phase effects and terminal pointer disagreement.
- Existing plan-refund and historical-correction conservation tests retain full
  cash, receivable/payable/suspense and document behavior.

Validation:

- `cargo check -p oxsum-core --tests`: passed.
- Final core `dispute_atomicity`, `refunds`, `plan_refunds`,
  `plan_usage_corrections`: **49 passed, 0 skipped**, nextest run
  `52f5f1c7-5492-4e7c-b06f-083293a7001c`, 14.166 seconds. Includes normal and
  corrected-source audit rollback, same-key/different-key concurrent loss, current
  role revocation, terminal pointer/audit disagreements and full source conservation.
- Server `refunds`: **9 passed, 0 skipped**, nextest run
  `7410fe29-24b1-4ec9-841d-e726fc1711d6`, 15.098 seconds. HTTP assertions pin
  whitespace-normalized replay, changed-reason conflict and terminal disagreement.
- Strict workspace Clippy (`--all-targets -- -D warnings`), pinned formatter,
  four OpenAPI contract tests and TypeScript/Vite build: passed.
- Initial new fixture loops reused business-scoped request keys; per-dispute keys
  corrected those test collisions before the complete 48- and final 49-test passes.
- Independent root source review found no introduced blocker; final rebase
  integration checks and protected PR integration remain pending.

No external PSP, browser, production correction or mixed-version write safety
was validated. Deployments must stop/drain old writers. Source-review conflicts
are deliberate retained boundaries; this unit provides no generic historical
dispute repair command.
