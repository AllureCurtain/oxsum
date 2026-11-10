# Exact plan dispute amounts (#1028)

Development acceptance based on main `2ee706019a02f24b4b8ed4b949b8c5254b27a178`.
The failure was discovered while implementing invoice correction/reissue #1027;
that separate workflow remains incomplete.

## Observed failure

A signed local-event fixture for a USD 2.50 plan (2,500,000 credit minor)
reported a USD 1.00 dispute. The original recorder ignored the partial amount
for `plan_period` and retained 2,500,000 rather than 1,000,000. Lost resolution
therefore removed the full retained Cash and restored excess customer debt.
[Original failing regression](baseline.txt) records the actual and expected
amounts. No actual provider or live payment was used.

## Change and evidence

Intake, case recording and resolution now check exact pinned event terms for
plan payments. Case recording compares the supplied coordinates with the retained
authenticated event and serializes merchant/case identity. Repeated notifications
do not increase contested amounts again. A historical full-amount case that
disagrees with its event refuses further resolution without changing its history.
Independent comparison captures the original event and independently converts
its amount, rather than certifying matching wrong journal/ledger figures.

- 73 focused daily-accounting/reconciliation checks passed, including policy 1/2
  partial cash loss, duplicate/concurrent case notifications, nonintegral fiat
  refusal and a deliberately changed original event against otherwise agreeing
  financial records.
- 100 related core integration checks passed (refunds, plan refunds, journal and
  statement lifecycle, merchant events and non-plan chargebacks).
- After extending resolution validation, nine affected library cases and a final
  historical wrong-amount refusal case passed. Original bad case and Cash remain
  unchanged on refusal.
- 18 existing HTTP refund/merchant/journal checks passed, including signed case
  identity, bad signature and unknown coordinate refusal, and recovery behavior.
- Core strict Clippy, five contract checks, generated TypeScript schema, frontend
  production build and pinned formatting passed. Frontend emitted its existing
  chunk-size advisory; no frontend behavior changed.

The old plan-refund fixture incorrectly sent credit minor as fiat minor. Its
helper now converts through the original order ratio and explicitly requires an
accepted receipt. Historical generated test data is not a supported webhook
format. These checks are development evidence, not genuine PSP, merged-main or
candidate certification. Independent review and protected integration remain.
