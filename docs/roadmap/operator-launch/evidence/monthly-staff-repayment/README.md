# Verified staff monthly repayment (#1015)

Branch `feat/monthly-staff-repayment` composes monthly issuance PR #1012 and
atomic monthly repayment PR #1014 on main `2ee70601`. The implementation gives
personal owner/finance sessions a customer-scoped verified repayment action.
Confirmation names the customer, month, exact credit amount, payment reference
and reason. Cash, credit restoration, FIFO standing/events, receipt reference,
immutable result and actor audit commit together.

Migration 0130 shares exact references across deployment manual receipts, staff
manual payment orders and staff monthly repayments. Upgrade locks source writes
until backfill and AFTER INSERT triggers are installed. Historical duplicates
reserve their reference without rewriting old receipts or orders. Existing
ON CONFLICT retries continue to recover the original source.

## Automated validation

- Prerequisite composition: 16 focused core/HTTP cases passed.
- Full workspace: **2,284 passed, six existing opt-ins skipped**, 351.519 seconds.
  This run preceded the final extension of the reference registry to staff
  manual orders; it is not represented as a full run of those later edits.
- Final affected selection after that extension: **52 passed**, covering
  `customer_statements`, `monthly_repayment_upgrade`, `collection*`,
  `bundle_upgrade`, `plan_payment_upgrade`, `refunds` and `payment_orders` in core
  and server. The 1,603 other tests were filtered out. Upgrade tests cover both
  singleton and historically conflicting references, unchanged old rows and
  old-writer exact retry.
- New regression coverage includes concurrent exact requests on one pooled
  connection, paid/archived recovery, current and queued-revoked authority,
  foreign source/customer pairs, changed intent, cross-path reference collisions,
  manual funding races and injected final-row failure rolling back all cash,
  standings, claims and actor audit. HTTP checks reject machine credentials,
  unknown request fields, client dates and invalid financial values.
- Strict core/server all-target Clippy, eight OpenAPI security/contract checks,
  type generation, production frontend build and pinned formatting passed. The
  existing Vite chunk-size warning remains. Unchanged full-suite coverage is
  reused according to the repository's testing-scope policy.

## Actual browser acceptance

Executed 2026-11-11 Asia/Shanghai (2026-11-10 UTC), using Ego-browser TaskSpace 5
and the embedded dashboard served by the local development binary. Synthetic
August and September usage created two issued documents with debts of 1,000,001
and 1,000,002 minor units. The fixture used ordinary core hold, settle, usage,
generate and finalize operations. No provider or PSP traffic occurred.

- Chinese desktop refused seven fractional digits before submission. A request
  one minor unit above August's remaining debt returned 400; its input remained
  editable and could be corrected. Keyboard Enter opened confirmation.
- The browser allowed the corrected August payment to commit, then a fetch shim
  deliberately discarded its successful response. Cancel, detail refresh and
  reopen retained and locked the original amount/reference/reason even after the
  document became paid. English mobile retry returned byte-identical result
  data and sent byte-identical request JSON. Keyboard Enter confirmed the retry.
- Reusing August's reference for September returned 409 without a new payment.
  Fields became editable; correcting the reference succeeded for 1,000,002 minor
  units. The success receipt remained visible after the document became paid.
- English 390×844 confirmation and successful detail were visually inspected.
  The body stayed 390 pixels wide; monthly tables were keyboard-focusable and
  scrolled within a 325-pixel region rather than expanding the page.
- Changing the fixture Finance user to Support removed monthly reads/actions.
  The role was restored afterward. The actual customer login then showed both
  months paid with zero outstanding, retained original usage and proof anchors,
  and `creditUsedMinor=0`, `availableMinor=creditLimitMinor=5000000`. All available
  value was undrawn credit; no customer wallet funds were manufactured.
- Database verification found **two unique receipts totaling 2,000,003 minor**,
  each with one reference claim, one ledger entry and one actor audit. Each entry
  contains exactly Cash debit and CreditLine credit for its receipt amount.
  Five browser submissions include the overpayment refusal, successful payment,
  exact successful replay, duplicate-reference refusal and second payment.

Evidence: [browser requests/results](browser-actions.json),
[customer response](customer-result.json), [ledger counts/postings](ledger-result.json),
[source and artifact identities](artifacts.json).
Screenshots: [Chinese confirmation](confirmation-zh.png),
[English mobile retry](retry-en-mobile.png), [paid mobile receipt](paid-en-mobile.png),
[Support boundary](support-boundary.png), [customer statements](customer-paid.png),
[customer detail](customer-detail.png).

TaskSpace 5 was closed with `finish({keep: []})`; the task-owned listener on
127.0.0.1:18815 was stopped. The temporary fixture example was removed from the
repository. Credentials remain outside tracked evidence.

## Remaining integration gates

Independent review, prerequisite protected integration, main/post-merge CI and
exact-candidate acceptance remain pending. This development fixture is not real
bank, PSP or release certification. The shared provider budget remains 2/5
attempts, 16,896 CNY millionths reserved and 52 millionths estimated usage.
