# Source-specific mixed statement voids

Issue #927; prerequisite PR #919 (`28d2b716`), main `5285fc0b`.
Migration 0125 follows the independent reserved 0120–0124 changes.

Finance uses the existing journal void operation with unchanged FundsWrite,
reason/idempotency/audit requirements. It verifies financial history, then selects
the original journal behind each charge, repayment or returned payment. Exact
opposite statement lines and immutable original/reversal/journal/canonical-entry
associations commit with the financial void and repayment-funded payable shares.
Unrelated sources retain their debt, counters and refund ownership. Cash never
moves. Entirely reversed documents become void; mixed documents retain their
remaining state and dunning. The shared subscription/all-plan/append lock order
also serializes other-slice payment intents; any live intent on an affected
document refuses before mutation.

Independent source comparison verifies each opposite and canonical void, original
repayment/dispute sources, and the original period's exact journal charges/payment
sources before excluding the correction. Equal-and-opposite document tampering
cannot substitute for journal evidence. Fresh frozen artifacts commit source-void
associations and referenced entries; existing frozen bytes are unchanged. Generic
offline verification checks committed integrity, not the new business policy.

Development validation executed so far:

- 44 financial source checks passed (16.601s), including both v1/v2 void orders,
  unpaid/partial/full mixed repayment, subsequent refund/loss and second-source
  void, cash invariance and remaining dunning/quote source.
- 63 existing journal/statement/refund/identity/upgrade/lock checks passed (14.909s).
- Other-slice live intent refusal and an actual create-versus-void race, injected
  association-write rollback, unchanged old export and valid new export, immutable
  association and corrupted canonical hash checks passed.
- Four OpenAPI checks, regenerated types and production frontend build passed;
  the existing large-bundle warning remains.
- One full workspace run executed all 2,174 ordinary tests (347.789s): 2,173
  passed and the whole-statement mixed-void refusal regression failed. The
  corrected statement entry point preserves that refusal and directs finance to
  the journal operation. All 67 final financial/recognition tests then passed
  (24.859s), including the unchanged regression and strengthened original-source
  checks. All 34 final statement/journal/core/HTTP tests passed (7.822s), including
  malformed source conflict mapping. Unchanged full-run passes are reused; all
  2,174 ordinary cases now have passing evidence. Six existing opt-ins remain out
  of scope. This is not a claim that the initial full invocation was green.
- Strict core all-target Clippy passed (44.38s); pinned format and diff checks
  passed. Final contract/build checks passed after the bilingual journal-void
  confirmation text was updated. No server Rust code or verifier code changed.

Independent review, protected integration and browser/candidate acceptance remain.
Reissued or inconsistent history still requires explicit review. A source void
never issues a replacement charge; that separate correction workflow and #777's
full recovery acceptance remain open. No external payment/provider action occurred
(global real-provider budget remains 0/5 attempts and CNY 0).
