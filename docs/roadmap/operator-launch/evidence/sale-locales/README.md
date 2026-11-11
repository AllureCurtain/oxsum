# Sale locale reacceptance (#994 / PR #996)

Actual Chinese browser acceptance in #992/#995 showed untranslated sale editing
and preview. This PR translates the editor, publication, activation, customer
preview, schedule states and exact-input/known-server validation. Protocol enum
values and financial parsing remain unchanged; unknown diagnostics stay visible.

On 2026-11-11 Asia/Shanghai, Ego-browser TaskSpace 6 used the existing isolated
browser fixture database and an actual local listener at 127.0.0.1:18816. No
model, PSP or email traffic occurred. Acceptance created a separate sale book and
assigned it only to the fixture owner's customer account.

The first reacceptance reproduced a feedback defect: after a language switch,
validation remained in its original language. The forms stored already-translated
error and success strings. Retain original errors/translation metadata and the
successful revision/version/schedule values instead, translating at render time.
The same fix covers draft, publication, activation, ending and preview feedback.

The repaired production frontend was reloaded in the same page and accepted:

- Seven-fraction input was rejected without mutation. Switching English → Chinese
  → English immediately translated that retained error. Input values were kept.
- Exact 1.000001-credit input saved revision 1; the successful save message followed
  a language switch. Publishing that exact revision produced immutable version 1,
  and the published-result message also switched without another mutation.
- Invalid 2026-02-29 UTC input refused in English and translated to Chinese.
  A conflicting business-default schedule returned the localized server conflict
  without replacing the existing active default. Input and reason remained.
- Assignment to the fixture owner's customer succeeded with a separate schedule.
  Its success message followed the language. Actual customer preview of 1,000,000
  input tokens and zero output returned exactly 1,000,001 minor units, using the
  assigned source, version 1, draft 1 and the recorded schedule identity.
- Chinese and English 390×844 preview screenshots were visually inspected. Both
  the viewport and body width were 390 pixels; all dynamic identifiers, counts,
  prices and server UTC values remained visible and unmodified.

Four existing exact-input/locale/interpolation tests, five OpenAPI contract tests,
TypeScript/Vite production build, pinned formatting and diff check passed after
repair. Existing Vite chunk-size warning remains. No Rust behavior changed; no
workspace test run was required for these frontend changes.

Evidence: [saved immutable history](saved-history.json),
[English result and locale assertions](browser-result.json),
[source/artifact identity](artifacts.json),
[Chinese schedule refusal](schedule-conflict-zh.png),
[Chinese mobile result](preview-zh-mobile.png),
[English mobile result](preview-en-mobile.png).

TaskSpace 6 was closed with `finish({keep: []})` and the task-owned listener was
stopped. Independent review, protected integration and final candidate acceptance
remain pending. No real-provider allowance was consumed.
