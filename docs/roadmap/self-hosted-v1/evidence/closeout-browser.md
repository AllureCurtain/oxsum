# Closeout: customer and staff browser acceptance

Issue [#688](https://github.com/AllureCurtain/oxsum/issues/688), A25.
Executed 2026-11-07 in ego-browser against an isolated PostgreSQL 17 database
and the real HTTP server on loopback. Desktop viewport: 1200 × 817; mobile:
390 × 844. The browser session and screenshot capture were driven manually
through the browser automation interface, not inferred from API tests.

## Code and fixture provenance

The initial server was based on `e21e16485a2d6b1ccf4409d564a2cce8df8e329f`.
The journey exposed and was resumed after the session-shell correction
in [#691](https://github.com/AllureCurtain/oxsum/pull/691) and the payment
worker correction in [#693](https://github.com/AllureCurtain/oxsum/pull/693).
Final server source was `ba6fea62247552e36e470d6f5d43f6234c2a6cb5`;
the frontend was built from the source changes accompanying this evidence,
based on `be5dd4c1e351ecd2f93c21f5317c026fb17d12cc`. This is incremental
defect/retest evidence, not a claim that every screenshot came from one
immutable release binary. Source comparison after integration found no backend/runtime difference
between that server commit and `813c788`, and no frontend source difference
between the final browser changes (`95fa2f2`) and `813c788`. The candidate
container build is still a separate artifact. Exact candidate checks are recorded
separately in [ACCEPTANCE-RESULTS](../ACCEPTANCE-RESULTS.md).

The retained [fixture source](artifacts/closeout-browser/browser-fixture.mjs)
records the setup and wire responses used. It creates a scratch database,
random local secrets, a loopback model/payment service and an isolated server
environment. Its original temporary paths are retained for provenance; copy
the built server to the indicated location before replaying. The runtime
configuration contains local session cookies and invitation tokens and must
not be committed. The SMTP endpoint is an unused loopback port; no mail or
outside contact is sent. Stop the server and fixture after testing; the
fixture's signal handler drops only its own named database.

- Owner setup, provider channel/price, merchant bootstrap and invitation
  creation use authenticated APIs. No database writes fabricate funding.
- Finance/support identities redeem customer invitations through APIs; the
  owner then appoints their existing user UUIDs as staff in the browser.
  This is staff appointment, not a staff invitation flow.
- The model stub emits one OpenAI-compatible stream with six input and two
  output tokens. It is not a paid model call or protocol certification.
- The payment stub implements `/payments`, `/refunds` and valid HMAC-SHA512
  callbacks to the real application. A local mock `HyperLoader.js` presents
  the Pay button. Browser script loading redirects only the fixture SDK URL
  `https://sdk.browser.example/v1` to the loopback stub. This does not exercise
  the real Hyperswitch SDK, provider authentication, a card or PSP money-out.

## Executed journey

| Surface | Action and observed result |
| --- | --- |
| Owner/context | Login, enter business context, appoint finance/support, return to customer portal and re-enter business. The corrected shell retains the valid staff session. |
| Customer invitation | Open the API-minted invitation, register with Tab/Enter submission, log in. Refocusing the browser no longer clears the form. Invalid invitation displays an error with submission disabled. |
| Checkout error | An account without a display currency shows a configuration error. Set USD, rate 1, two decimals through the authenticated organization API, then retry. |
| Funding | Create and pay a USD 1 wallet order. The original callback exposed paid-but-unfulfilled recovery delay; the new worker recovers it on startup. Create/pay a second USD 2 order after the fix: the page automatically changes to fulfilled without reload. |
| Consumption | Create a scoped key with a one-credit limit and `browser-chat` allowlist. The browser Chat page streams a reply; its separately minted chat key charges eight minor units. Requests/Bills show the corresponding settlement. |
| Verification | Fetch the authenticated proof for that actual settlement and paste its bundle and recorded hash into `/verify`. The browser WASM verifier accepts it, then rejects an all-zero expected hash. The `?entry=` URL does not auto-fill this paste interface. |
| Refund decisions | Customer requests the first order's full refund; finance declines it because part was consumed. Customer requests the unused second purchase; finance approves, the stub sends a signed success callback, and the UI shows succeeded plus the credit note. Audit shows both decisions. |
| Permissions | Customer business access is denied. Support sees its permitted setup/customer sections; an explicit refunds API probe returns 403. Final support customer detail makes no forbidden financial-table requests. |
| Narrow screen | Open/close navigation including Escape. Scroll the order table horizontally to reach amount and detail action, with a focusable outlined table region. Page width stays 390 px; table viewport 362 px and content 621 px. |
| Empty/error states | Billing without a plan is empty; invalid invitation is refused; support setup reports its one visible completed step and its Open link navigates to customers. |

Keyboard Tab/Enter submission, Escape and focus visibility were observed.
Horizontal scrolling was exercised, but keyboard-only horizontal scrolling
was not established; this is not a complete accessibility audit. Paid plan
purchase/renewal, disputes, overdue release and a complete business report
cycle were not driven through the browser in this run. Their domain/HTTP
suites remain separate evidence; A25 is scoped browser coverage, not a
blanket pass for every portal and console workflow.

## Accounting cross-check

Customer account: `4ba57837-660f-4869-844f-df3862f85e2c`.
Orders: `d6990faa-78e3-4b12-b941-3d655532c9ed` (USD 1) and
`831e7866-27cd-4519-8c42-f1512da04dd7` (USD 2).
Request: `fd3a3670-d071-4cbb-a323-c4133ca77e3e`.
Settlement: `64b6dfe6-5b5e-5fcd-9826-f33b1ed78269`.
Retained [actual proof](artifacts/closeout-browser/customer-proof.json) and
[sanitized accounting read](artifacts/closeout-browser/accounting.json).
Recorded hash:
`1682422814b90fd66e16c686a2b5c970bd95d8b3b1595f684a63fba7cbd824e0`.

Authenticated reads and a read-only database cross-check show both orders
fulfilled, one declined refund for 1,000,000 minor units, one succeeded
refund for 2,000,000, the succeeded eight-unit operation, and no open holds.
Final available balance is **999,992**, reserved **0**:
`3,000,000 purchased − 2,000,000 refunded − 8 usage = 999,992`.
The dashboard's `spentThisMonth` includes the refund and reports 2,000,008;
it is not a claim of that much model usage.

## Retained screenshots and verification

All screenshots contain isolated fixture identities, not customer data or
full credentials. They were visually inspected before retention.

| Evidence | Screenshot |
| --- | --- |
| Mobile amounts and row action reachable | [06](artifacts/closeout-browser/06-mobile-order-actions.png) |
| Actual billed stream | [07](artifacts/closeout-browser/07-billed-chat.png) |
| Genuine bill / wrong hash | [09](artifacts/closeout-browser/09-genuine-bill.png), [10](artifacts/closeout-browser/10-tampered-bill.png) |
| Automatic fulfillment | [14](artifacts/closeout-browser/14-checkout-auto-fulfilled.png) |
| Refund and credit note | [15](artifacts/closeout-browser/15-finance-refund-succeeded.png) |
| Capability-filtered setup and customer detail | [17](artifacts/closeout-browser/17-support-setup.png), [18](artifacts/closeout-browser/18-customer-detail-support.png) |
| Customer sees succeeded refund | [19](artifacts/closeout-browser/19-customer-refunded.png) |
| Invalid invitation | [21](artifacts/closeout-browser/21-invalid-invitation.png) |

`npm run build` (TypeScript and Vite) passed after the final refinements.
The focused backend fixes have their own checks: four context/console tests
for #691; 30 PostgreSQL-backed payment/refund/event/job tests, strict Clippy
and a 1,780-pass workspace selection for #693. These checks do not turn the
mock payment/model services into external certification.

After integration, the SDK tarball from candidate run `42338752086` was
installed into a fresh temporary project and verified this retained actual
bill offline. Correct hash passed, an all-zero hash failed, and a throwing
`fetch` stub recorded zero network attempts. See the
[recorded verdicts](artifacts/closeout-candidate/actual-bill-verification.json).
This ties the retained bill to the final candidate verifier without claiming
a second browser journey against the final container image.
