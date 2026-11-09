# Scoped application wallet and bundle checkout

Issue #930; N21/J18. Branch `feat/application-checkout`, based on PR #928
(`88f2afb5`). Migration 0126 follows the separately reserved 0125. Development
PostgreSQL 17 and a real loopback HTTP provider fixture only; zero external
payment or model attempts.

An explicit application-only `checkoutWrite` grant quotes and creates wallet
funding or a published bundle. Quotes expose the merchant name/test-live mode,
exact immutable route/version, business collection revision and integer fiat,
purchased and promotional terms. Callers cannot set conversion or free credits.
Creation rechecks current credential, namespace, active customer and exact quote
inside the existing payment transaction, before either replay or new creation.
The application association and exact request commit with the order and binding.

Operation keys have an application-specific domain independent of portal orders.
Unchanged retries after credential or merchant rotation return the original
order/version. Changed terms or destinations conflict. Scoped history includes
only that application's orders; archive permits history and authentic late
callbacks but refuses new/resumed initiation. Absolute HTTPS or literal loopback
HTTP destinations reject userinfo, fragments, whitespace, controls and backslashes.
Both application and linked-customer portal recovery retain the original URLs.

The production checkout driver calls the provider only after durable authorization.
Revocation afterward cannot undo an already authorized operation. Unknown external
outcomes retry the same order UUID and never create a second intent. A checkout
response does not establish payment: the existing authenticated event and durable
fulfillment boundary grant the original funds exactly once.

Executed validation:

- All 2,175 ordinary workspace tests passed (343.072s); six existing opt-in tests
  were skipped. No new skips or external certification claims.
- 22 affected merchant-route, payment-order, bundle and application core/HTTP
  checks passed after final quote/destination changes (4.395s).
- Six final application core/HTTP checks passed (1.732s), including concurrent
  replay, credential/merchant rotation, stale/foreign/tampered quotes, bundle
  retirement, exact returned destinations, revoked/archived preflight races,
  rollback of association failure, isolated app/portal namespaces, bounded
  history, and verified once-only fulfillment without prepayment wallet writes.
- The HTTP provider fixture loses each intent's first response. Both machine
  recovery and a verified-consent linked customer's portal recovery reuse the
  original provider identity and return URL; repeated resume issues no extra
  provider request. Missing/revoked credentials, wrong scope and archived
  initiation fail while authorized historical reads remain available.
- Four OpenAPI checks, generated TypeScript, frontend production build and eight
  frontend grant/fragment checks passed. The existing large-bundle warning remains.
- Strict all-target Clippy for core/server and pinned formatting passed. Unchanged
  full-suite evidence is reused for the final targeted changes.

Independent review, protected integration, browser scope-form acceptance and exact
candidate evidence remain pending. The previous browser task space is unavailable;
no replacement or user-owned space was used. Assigned-plan checkout is #931;
named statement debt, evidence publication, signed lifecycle notifications and the
complete external app remain N21 follow-ups. This PR does not close N21/J18.
