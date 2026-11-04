# Product contract

Status: full implementation target; design defaults are listed explicitly. User requirements take precedence over defaults. No oxsum-project monetization is assumed.

## Actors and funds

One self-hosted business sells AI service. Customers may be individuals or teams. Staff configure providers/prices and handle commercial operations. The deployment administrator installs/upgrades and retains a break-glass recovery path.

Example: provider expense is 6 comparable credit units, customer sale is 10, estimated business usage margin is 4 before fees/tax/adjustments. Customer payment goes to the business merchant. Provider bills the business externally. oxsum meters and records; it does not receive a second payment.

Customer prepaid credit is a liability of the business. Subscription quota is a service entitlement, not refundable cash. Rated usage, quota consumption, promotional spending, purchased-credit spending, receivable draw, payments and recognized revenue are separate quantities.

## First-release experience

Install a pinned image/Compose profile → initialize business and owner → configure domain/contact/provider/collection → publish models/prices/plans → invite customers or open registration → customer funds account or buys a plan → key/SDK call → reserve/settle → inspect and independently verify bill → renew/pay/refund/reconcile as appropriate.

Default to one business per deployment; multiple customer accounts are required. Customer teams reuse existing organization membership. Do not introduce a seller marketplace, business-to-business resale hierarchy or cross-business pooled wallet.

## Required capabilities and traceability

| Requirement | Required behavior | Tasks |
| --- | --- | --- |
| R01 Install and operate | Secure initialization, seller branding/contact/domain, SMTP/setup validation, documented core/payment deployment profiles, version and health | T02,T03,T23,T28 |
| R02 Staff and customers | Separate staff privileges, invited/self-registered customer accounts, customer teams, stable external ID, suspension/archive, audit | T01,T02,T03,T04,T05,T24 |
| R03 Provider operation | Encrypted provider keys, connectivity tests, model catalog/aliases, credential pools/rotation, bounded fallback/retry, compatibility matrix | T06,T07,T13,T26 |
| R04 Sale terms | Immutable published rate versions, defaults/group/customer overrides, scheduled rates, discounts, estimate, cost-separated pricing | T08,T09,T10,T22 |
| R05 Consumption | Purchased and promotional sources, key/customer limits, hold/settle/release, zero-charge and fully entitled turns, tasks/cache/replay/recovery | T05,T10,T11,T12,T13,T18,T19 |
| R06 Money collection | Checkout into operator merchant, verified paid credit, manual receipt/grant authority, duplicate/out-of-order handling, refunds/disputes | T01,T14,T15,T16,T17,T23,T25 |
| R07 Commercial plans | Prepaid bundles, expiring promotional grants, one recurring quota plan, paid renewal, overage, cancellation and expiry | T18,T19,T20,T23,T25 |
| R08 Postpaid | Approved credit, statements/due date, partial/full allocation, overdue notifications/suspension/restoration, corrections | T21,T24,T25 |
| R09 Reports | Usage versus money reporting, payment/receivable/refund totals, revenue/cost/margin coverage and exception resolution | T17,T20,T21,T22,T24 |
| R10 Evidence | Correct price/funding/account proof, statement/report scope, customer privacy, signed heads and compatibility, packaged independent verifier | T09,T11,T12,T13,T25,T29 |
| R11 Integrations | Scoped management/metering APIs, signed/retryable outbound webhooks, idempotency and working SDK examples | T05,T13,T23,T24,T26 |
| R12 Release | Fault/concurrency/security tests, measured capacity, backup/restore/upgrade, versioned artifact, docs/demo, independent pilots | T27,T28,T29,T30,T31 |

## Roles (minimum capability sets)

| Role | Allowed | Excluded |
| --- | --- | --- |
| Business owner | All business operations; appoint staff, merchant/secret settings, ownership transfer | Customer identity impersonation without an explicit audited mechanism |
| Business admin | Customer/model/catalog configuration and operational management | Owner transfer, staff privilege escalation, merchant secret changes unless delegated by explicit capability |
| Finance | Customer commercial terms, credit/grants/manual receipts, statements/refunds, financial reports | Provider secrets, staff management, routing changes |
| Support | Customer/request read, documented suspension/support notes | Money creation/refund, price/credit changes, secrets, private payloads |
| Viewer | Authorized redacted aggregate/read views | All mutations and secrets |
| Customer owner/admin/member | Reuse customer-team rules; owner/admin manage team keys/members; scoped customer reads and checkout | Staff/merchant/provider APIs, manual credit, arbitrary usage injection |
| Consumption key | Gateway, allowed models, own key's usage/status/receipt where specified | Customer-wide exports/team management, money changes, staff operations |
| Integration credential | Explicit business scopes and selected customer set for management or metering | Unscoped global power; funds-write scopes require owner issuance and audit |

Use deny-by-default capabilities in core; hiding UI controls is insufficient. Existing API keys retain gateway/compatible read access but lose unsafe organization-wide money authority. Existing customer sessions may request payment, never declare it paid.

## Commercial defaults

- Money uses 1 credit = 1,000,000 integer minor units. A business chooses one settlement currency and a fixed rational credit conversion before live collection. Existing display-only currency remains presentation, never an implicit conversion. Existing funded currencies cannot be relabeled; historical transactions pin their conversion.
- One active price group/book assignment per customer. Precedence: customer override → assigned book → default book. No ambiguous stacked assignment. Discounts pin once; deterministic integer rounding; ceiling at documented total stage.
- One active recurring quota plan per customer. New paid periods require confirmed renewal/payment or an explicit audited complimentary grant. Manual payment renewal must be supported; automatic provider subscription renewal is optional per certified connector, never falsely advertised.
- New plans support monthly anniversary periods in UTC with documented month-end clamping. Existing legacy calendar-period plans retain their exact policy. New policy is versioned; no silent time-boundary change.
- Cancellation and changes take effect at next boundary. No initial proration, carryover, overlapping paid plans or arbitrary benefit stacking. Unused recurring quota expires; overage is opt-in and price-pinned.
- Admission funding order: valid plan allowance → eligible promotional grants (earliest expiry then oldest grant) → purchased credit → approved credit. Allocation pins lot/period; later expiry cannot steal in-flight reservation. Released expired grants do not revive spendable credit.
- Purchased credit does not expire in release one. Promotional grants can expire and cannot be cashed out. A purchased bundle is a paid deposit plus a separately identified promotional grant; show both before sale.
- Credit/customer budgets include in-flight committed amounts; new suspension blocks admission while existing settlement/refund/payment recovery continues. Automated dunning lifts only its own suspension.
- Default registration is invite-only. Owner can enable self-registration with verification/abuse controls; customer registration never grants staff authority. Staff owner initializes through one-time setup, not public signup.

## Billing failure policy

Missing price/permission/funds refuses before upstream. No output and no billable usage releases under the declared outcome. Cancellation charges documented observed/estimated delivered usage and labels its provenance. Actual above cap charges at most authorized cap and records an anomaly. Provider attempts with ambiguous outcome are not replayed blindly. Unresolved operations remain visible/recoverable.

No public/customer endpoint can directly increase purchased balance. Manual receipt requires staff authority and a reason/reference. Promotional adjustments must not masquerade as cash received. Original charges/payments are retained; credit notes, refunds, disputes and write-offs are linked events.

## Surface and protocol scope

Business console: Overview, Customers, Models/channels, Prices/plans, Requests, Payments/refunds, Statements/receivables, Reports/reconciliation, Integrations, Staff/settings.

Customer portal: Getting started, Models/prices, Balance/plans, Keys, Usage/requests, Bills/payments, Account/help. Reuse React components and existing authentication. Preserve project design rules, existing locale conventions and accessibility. English docs/code/comments; existing English/Chinese UI locale coverage must not regress.

Revalidate existing Chat Completions, Responses, Anthropic Messages, Gemini, embeddings, rerank, generic task and metering surfaces. Publish exact provider/SDK/parameter coverage. Generic async plumbing does not claim universal image/audio/video support. Keep unknown modalities off the supported list until tested.

## Release boundaries

Required: complete advertised paths including failure/refund/overdue/recovery; certified primary collection, independent verification, reproducible installation and external pilot evidence. Advanced tax invoicing, multi-currency wallets, proration, revenue-sharing payouts, multi-region, arbitrary custom-domain automation and new modalities are separate extensions.

Software implementation can finish before live payment certification/pilots. State those external gates explicitly; do not remove them from release acceptance or use their absence to stop unrelated implementation.
