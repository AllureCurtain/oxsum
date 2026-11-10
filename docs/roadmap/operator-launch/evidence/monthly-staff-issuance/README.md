# Reviewed Business monthly issuance (#1011)

PR [#1012](https://github.com/AllureCurtain/oxsum/pull/1012).
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

## Browser acceptance completed

On 2026-11-10 UTC (2026-11-11 Asia/Shanghai), the user authorized a replacement
for the disappeared browser task space. Ego-browser TaskSpace 3 used one page
against the actual local listener with a Finance personal session. No public
provider, payment or email service was called.

- Chinese desktop: generated October's canonical 1.000001-credit draft, reviewed
  the original figures and 30-day terms, and opened confirmation using keyboard
  Enter. A late two-minor usage row caused the old review to refuse with 409;
  document and proof remained unissued until the draft was refreshed.
- English 390×844 mobile: refreshed figures were exactly 1.000003 credits. A
  browser fetch shim let finalization commit and then deliberately discarded the
  successful response. Cancel/reopen retained the reason and complete intent;
  the reason stayed disabled. Retry sent byte-identical JSON and returned the
  same receipt. Database verification found one October anchor, mutation and
  actor audit. [Requests](browser-actions.json), [results](browser-result.json).
- Browser inspection found three feedback defects: a success string did not
  follow a language switch, a stale-review warning persisted after refresh, and
  the issued-state early return hid the success message. These were repaired in
  the same feature: retain translation keys, bind stale feedback to its rejected
  digest, and preserve successful issuance feedback on the issued document.
- A separate August canonical fixture repeated generate/stale-review/refresh/
  issue on the repaired frontend. The stale warning cleared after refreshed
  review, Chinese/English success messages switched correctly, the issued detail
  retained its success message and removed the issue button, and keyboard Enter
  expanded proof identifiers. The final 390-pixel page had no body overflow;
  tables remained focusable and horizontally scrollable (325-pixel viewport,
  640/480-pixel monthly tables).
- Changing the fixture Finance user to Support hid both monthly information and
  actions. The fixture role was restored afterward. Both languages' confirmations
  and the mobile issued detail were visually inspected. The frontend production
  build and all seven contract checks passed after the feedback fix; unchanged
  backend test evidence above was reused.

Screenshots: [Chinese confirmation](review-zh-desktop.png),
[stale review refusal](stale-zh.png), [mobile English confirmation](review-en-mobile.png),
[retained retry](retry-en-mobile.png), [issued mobile detail](issued-detail-en-mobile.png),
[support boundary](support-no-monthly.png). Final local artifact identities are
recorded in [browser-artifacts.json](browser-artifacts.json).

TaskSpace 3 and its only page were closed with `finish({keep: []})` after
acceptance. Private credentials remain outside tracked evidence.

## Remaining gates

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
