# Business collection authority (#765)

Development evidence on the collection-authority branch based on `0314cbd`,
2026-11-08. No final-candidate, browser, real-model or payment certification claim.

## Defect and outcome

Payment orders previously derived purchased credit from a customer-editable
presentation preference and accepted a mismatched submitted fiat code. New
collection policy belongs to the Business owner, with revisioned audit receipts.
Provider and finance receipt creation resolve it inside their transaction;
personal display edits cannot reprice purchased credit. Already-created orders
and replays keep their original amounts/rates. Currency history prevents
relabeling, including legacy USD rail bindings and contradictory old currencies.
Migration 0101 leaves historical rows and ledger entries untouched.

## Executed validation

- Contract first, regenerated TypeScript schema, formatter and strict workspace
  Clippy pass. React production build and exact decimal/quote Node test pass.
- Initial focused core collection/payment/refund/identity selection: 29 passes.
- Upgrade/race selection and HTTP permission fixtures were corrected and pass.
- Final boundary selection: 18 PostgreSQL tests pass, no skips, including a
  first-payment/currency-change race, changed-rate/stale-quote race, wrong currency,
  customer display tampering, zero-credit rounding rejection, finance versus
  owner capabilities, immutable audit/replay and a real pre-0101 mixed-currency
  migration. The fixture's retained JSON is byte-equivalent after migration.
- First complete workspace run: 1,813 passed, 6 explicit opt-ins skipped,
  223.769 seconds. After adding the zero-credit guard and first-currency race,
  the final run executed 1,814: 1,813 passed, one existing job fixture failed,
  6 opt-ins skipped, 220.210 seconds. **This final full run is not green.**
- The failure was `faults::a_failed_job_backs_off_then_dies_and_the_kind_recovers`
  on its first claim. Its host-current enqueue timestamp can be in the database's
  future (199/200 direct probes reproduced that relation); its entire 17-test
  faults binary then passed unchanged. Independent #770 anchors the fixture to
  the database clock without changing production scheduling. Integrate that fix
  and rerun complete CI on the combined candidate; do not transfer the earlier
  workspace pass to this final source.
- Final server build passes. Additional actual-listener example checks exercise
  the development binary with a private database and SDK artifact; they make no
  paid provider call. Browser save/reload/conflict acceptance remains pending
  because the task browser space is unavailable.

Logs are retained in ignored `local-notes/operator-launch/collection-*.log`.
The exact candidate SHA and release artifact do not exist yet. Required genuine
review and normal protected merge remain pending. N10 commercial offer authority
(#768) remains separate; this change does not make arbitrary bundle promotions
or plan-period amounts authoritative.
