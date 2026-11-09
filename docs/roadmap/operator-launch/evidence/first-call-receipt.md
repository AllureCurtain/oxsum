# Immediate first-call receipt readiness

Issue: #909, slice of #901 / N09 / J08. Base: `5285fc0b`.
Environment: isolated local PostgreSQL, actual development HTTP listener and
embedded production frontend. Source-built WASM verifier; no real upstream or
payment requests. Browser fixture used canonical zero-charge settlements; the
HTTP regression also executed actual local-stub gateway calls funded by quota
and by a free price.

`GET /api/v1/org/readiness/receipt` searches at most 32 scoped successful
accounting candidates and verifies canonical identity, actor, charge arithmetic,
wallet revenue allocation and inclusion. Total priced usage and wallet charge
are separate: subscription-funded usage can have a nonzero price and zero
wallet charge. Missing or inconsistent evidence cannot mark ready. Later
refusals/errors do not conceal an earlier eligible receipt.

The request and bill checklist steps use this result. They need no period report
or report link. `/verify?entry=<uuid>` now loads the exact receipt under a current
personal customer session; it previously ignored the entry and showed an empty
form. Private evidence is never cached. Foreign/member-invisible entries are
404, absent sessions are 401, explicit machine headers are 403, malformed IDs
are 400. The ordinary pasted-proof and public sample modes remain public.
The prefilled hash is explicitly identified as supplied by the same server;
this is not independent head trust or provider-usage attestation.

Executed checks:

- Full real PostgreSQL workspace run: **2,156 passed, 6 existing opt-ins skipped**,
  389.412 seconds test runtime. Subsequent verification-page and quota-allocation
  fixes used affected focused coverage, following the evidence reuse policy.
- Final 59 core/HTTP/gateway/verification-page checks passed, zero skips. Coverage
  includes 13 later failed requests, free and fully quota-funded actual gateway
  calls, member/other-customer isolation, missing/malformed/mismatched evidence,
  bounded exhaustion, read-without-accounting-writes and private cache headers.
- Strict all-target Clippy passed for core, server and web; pinned formatting,
  four OpenAPI checks, codegen, production frontend build and two frontend state
  behavior groups passed.
- Source-built WASM and actual browser: a prefilled canonical receipt passed;
  an altered hash failed locally while the entry URL remained unchanged.
- Actual checklist browser checks covered English/Chinese, desktop/390-pixel
  layout, empty and incomplete evidence, refresh, exact proof link, and keyboard
  Tab focus (visible 2px outline) plus Enter navigation. Screenshots are in
  [first-call-receipt/](first-call-receipt/).

The other three readiness steps still have their old model/key/funding logic;
#901 owns replacing those conditions. The full journey, independently reviewed
protected integration, exact release candidate and external certification remain
separate pending gates. This evidence is a development fixture, not a completed
first-install or actual-provider journey.
