# Implementation spec — issue #507: console i18n

Goal: the React dashboard is English-only. Extract its user-facing strings for
translation and ship a second language (zh-CN) — a commercial checkbox for a
Chinese-market product, not structure for its own sake. i18next +
react-i18next is the standard; wire it through `web/src/i18n.ts`, ship
`web/src/locales/{en,zh-CN}.json`, add a header language switcher defaulting to
`navigator.language` persisted per session.

Work only inside this worktree (`feat/console-i18n`), branch
`feat/console-i18n`. One commit per logical step: i18n scaffold + deps,
en/zh-CN locale files, per-page extraction, switcher, docs. Do not open the PR
— the parent reviews the diff.

## Scope — mechanical but real; keep it honest

- Framework: `i18next` + `react-i18next` + `i18next-browser-languagedetector`
  (standard trio). Add to `web/package.json` deps; `npm --prefix web install`.
- `web/src/i18n.ts`: init with `en` as fallback, `zh-CN` resources, detector
  order `localStorage` → `navigator`, `returnEmptyString: false`,
  `interpolation: { escapeValue: false }` (React already escapes).
- `web/src/locales/en.json` + `zh-CN.json`: namespace by page/domain
  (`nav`, `overview`, `requests`, `keys`, `bills`, `usage`, `settings`,
  `auth`, `admin`, `common`, …). Flat-ish keys like `requests.filters.model`.
- Coverage: every user-facing string on every page — nav labels, page titles,
  form labels/hints/placeholders, table headers, buttons, status badges,
  empty states, error/banner text, toasts. Replace inline literals with
  `t("page.key")` via `useTranslation()`. Long descriptive paragraphs keep
  their full text in the locale file (don't truncate meaning).
- Language switcher: a small control in the dashboard header (AppShell /
  wherever the top nav lives) — `EN` / `中文` toggle, writes
  `localStorage['i18nextLng']`, calls `i18n.changeLanguage`. Persisted per
  session/user; browser `navigator.language` is the default.
- `/verify` is server-rendered (`crates/web`) — leave it English for v1; the
  issue says decide, and the Rust verdict strings stay English anyway
  (technical evidence, the protocol language). Do NOT translate the verify
  crate's verdict output.
- Admin-only pages stay English until a customer asks (the issue says so) —
  you may skip translating `web/src/pages/admin/*` strings, but the switcher
  and the shared chrome still work there.

## Conventions

- TypeScript: `t()` returns `string`; for interpolation use
  `t("k", { n })` / `{{var}}` placeholders, plurals via i18next's `_one`/`_other`
  only where a count actually renders.
- Keep the existing component vocabulary; the switcher should match the
  header's other controls (check how the AppShell renders nav/controls — reuse
  a `Button`/`select`/icon-button pattern that fits).
- Keys are kebab/dot-namespaced, English text is the `en` source of truth;
  zh-CN strings must be real translations, not transliterations or machine
  placeholders. Write natural Simplified Chinese for the zh-CN file — domain
  terms like "balance"/"hold"/"settlement"/"API key"/"webhook"/"channel"/"model"
  get sensible Chinese (余额 / 冻结 / 结算 / API 密钥 / Webhook / 渠道 / 模型).
- Strings embedded in non-component helpers (error mappers, formatters) get
  translated where they're rendered; a pure-formatting helper that emits
  prose may take the `t` fn or move to the component.

## Verify before returning

- `npm --prefix web install` brings the three i18next packages.
- `npm --prefix web run build` (tsc + vite) clean — no missing-key type
  errors, no unused imports.
- `npm --prefix web run codegen` is NOT needed (no contract change) — skip it.
- Spot-check: `web/src/pages/Login.tsx` (or auth pages) renders `t()` for its
  strings; the switcher toggles `document.documentElement.lang` or the i18n
  language and the visible text changes (assert in code, not by running a
  browser).
- No lint config to run beyond tsc unless the repo has one — check
  `package.json` scripts.

## Docs (same commits)

- `web/README.md` — the i18n setup, where locale files live, how to add a
  language.
- `docs/user-guide.md` — the language switcher exists.
- `docs/decisions.md` — append: i18next + zh-CN as the second language, en
  source of truth, verify page + admin pages stay English for v1.
- `TODO.md` — mark issue #507.

## Report

- Deps added, files changed, the locale namespace layout, how many strings
  extracted (rough count), which pages fully translated vs which stayed
  English (admin), the switcher's location, verification results, deviations.
