# Application assigned-plan checkout

Issue #931; branch `feat/application-plan-checkout`, based on PR #932
(`84e365c0`). This extends the existing application-only `checkoutWrite` grant
with `planPeriod`; no new schema migration, grant or entitlement writer.

An application selects only its own already assigned `customerPlanId`. The quote
retains `PlanPaymentTerms` (sold version/fee, period, conversion and fiat ceiling),
merchant route/name/test-live mode and zero wallet/promo credits. The existing
order transaction rechecks current application authority and the exact quote,
then uses the original plan fingerprint and paid activation path. Outstanding
portal or application intents prevent a second period payment; the generic
refusal does not disclose another caller's order UUID. Scoped history/resume
recovers only this application's orders and original URLs. Missing optional
plan fields are omitted from serialization to preserve retained wallet/bundle
request equality across this extension.

Development validation:

- 16 core/HTTP tests passed (3.894s): current v2 initial activation, exact
  1,000,001-credit-minor fee versus 101-fiat-minor ceiling, zero wallet credit,
  once-only fulfillment, concurrent different operation keys, stale/tampered
  terms, foreign/free/scheduled/cancelled plans, one standing portal intent,
  historical v1 boundary renewal, and old wallet JSON replay compatibility.
- The real loopback provider fixture loses the first plan checkout response,
  then recovers the same provider identity, exact price and original URL through
  machine HTTP using a rotated credential; checkout alone grants no period.
- One initial test attempted to move an accounted v2 period boundary and was
  correctly rejected by the immutable schedule trigger. The final suite retains
  the v2 activation test unchanged and uses the existing historical v1 fixture
  for simulated elapsed time. No production guard was weakened.
- Strict core/server all-target Clippy passed (1m52s), pinned formatting, four
  contract checks, generated TypeScript and frontend build passed. The existing
  large-bundle warning remains.
- Reuse #930's 2,175 passing workspace cases and final affected coverage for
  unchanged code. Production changes here are limited to the core application
  quote/types; shared payment, plan, ledger and server route implementation is
  unchanged. The focused suite includes the existing plan-payment regressions.

Independent review/protected integration, browser and exact candidate gates remain
pending. No external payment/model attempt occurred. Named statement debt,
evidence publication, signed notifications and the complete J18 app remain open.
