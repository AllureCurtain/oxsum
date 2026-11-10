# Reviewed Business monthly issuance (#1011)

Draft PR [#1012](https://github.com/AllureCurtain/oxsum/pull/1012).
Implementation source: `163bd3e9`.
Branch: `feat/monthly-staff-issuance`, created from main `2ee70601` and composed
with monthly read PR #1008 (`39acc911`) and atomic writer PR #1010 (`1e58bae5`).
Migration 0129 adds immutable action-result coordinates. This branch needs both
prerequisites before protected integration.

Owner/finance personal sessions can generate a closed UTC month and finalize a
customer-bound draft after reviewing its exact displayed figures, itemization,
terms and current UTC day. Business authority is rechecked in the writing
transaction after wallet preflight. A stale review rolls back anchor/document
changes; audit failure rolls back all effects. Historical replay preserves its
original result even for an empty month or an already-issued statement. A new
archived-customer action refuses while current authority gates earlier recovery.

## Executed validation

- Initial related core/HTTP/monthly/proof selection: **37/37 passed**.
- Full workspace: **2,270 executed; 2,268 passed, 2 failed, 6 existing opt-ins
  skipped**, 327.194 seconds. One failure was the newly added table missing from
  the explicit identity migration inventory; that assertion was updated. The
  other is unchanged `bundle_upgrade::upgrade_retains_fulfilled_history_and_fences_pending_bundles_and_old_writers`,
  tracked by PR #975 / #1002.
- Final affected selection: **10/10 passed**, including concurrent/repeated
  migrations, one-connection operation, current owner/finance role and context,
  revoked waiting writer, foreign document/customer pairs, machine credentials,
  unknown request fields and client financial dates, empty-month replay, changed
  key intent, exact racing finalization, immutable result rows, archived retries,
  late usage/changed terms/prior-UTC-day review refusal and injected audit failure.
  The known bundle failure remains; the full run is not described as green.
- Strict all-target core/server Clippy, pinned formatting and diff checks passed.
- Seven contract checks, code generation and production frontend build passed.
- Actual local development HTTP listener generated and issued a canonical
  **1.000001-credit** fixture. Discarding the successful response body then
  retrying twice returned the same receipt. Database reads found **one anchor,
  one mutation, one audit**. See [http-acceptance.json](http-acceptance.json) and
  [artifact hashes](artifacts.json). That listener precedes the final additional
  foreign-document preflight check, covered in final automated HTTP checks.

## Remaining acceptance

Browser UI acceptance did **not** run: resuming the established ego-browser
TaskSpace 2 returned `task space not found: 2`. The browser skill forbids creating
a replacement space to bypass an unexpected lost-space state without user input.
The bilingual UI builds, but desktop/mobile/keyboard confirmation and retained
form behavior still need actual browser execution. The local listener and fixture
are retained at `/tmp/oxsum-monthly-staff-browser-1011`, port 18811. Private
credentials remain outside tracked evidence.

Independent review, prerequisite integration, protected merge and exact-candidate
acceptance remain pending. This changes no monthly repayment or plan reissue
policy. No real PSP, external messages, public release or new provider calls
were performed. Fixture results are not release certification.
