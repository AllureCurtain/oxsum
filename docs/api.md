# API conventions

Field definitions for each endpoint are authoritative in `crates/server/openapi.yaml`; this file only records general conventions.

Ledger evidence descriptions (#873) are bounded to 16,384 Unicode scalar values,
at most 65,536 UTF-8 bytes before JSON escaping. Settlement version 14, the
canonical encoding and all existing hashes stay unchanged. Subscription and
cache provenance coexist in the complete receipt. Writers, server readers and
offline/browser verifiers share the bound; older 512-character readers cannot
read longer new entries. Arbitrary configured guardrail-output budgeting is a
separate admission requirement tracked by #875.

Non-plan refund effects (#841): `OrderRefund` and
`OrderCreditNote` expose nullable `cashEffect`. Its immutable verified Cash return
is separate from approval and note state, so late terminal and unapproved cash
remain visible without fabricating approval or changing historical records.
Complete Cash equals proven original Wallet take-back plus review suspense;
historical Wallet restitution is a separate non-Cash correction. Null means no
effect verified under this policy, not zero returned. A retained effect fences
approval, provider retries, refundable source reuse and unperformed grants.
The [local recovery command](nonplan-refund-recovery.md) requires exact receipt
evidence, current owner/finance authority and an unchanged preview revision on
apply. Its `nonplan_refund.recover` finance audit names the actor and outcome.

Business setup readiness (#843): `GET /api/v1/business/readiness` requires a
current personal session in Business context with `overviewRead`. It returns
`checkedAt`, `ready` and five `{key,state,action}` facts. `state` is `ready`,
`todo` or `restricted`; `action` is an existing manageable business section or
null. Restricted facts never count as ready. Profile and admission are overview
facts; supply requires `catalogWrite`, sale terms owner/admin/finance, and
collection `reportsRead`. Actions additionally require the relevant write
capability. The endpoint performs no external I/O or completion mutation and
never returns supplier/merchant secrets, endpoints or commercial values.
Configuration readiness is not provider/PSP certification or release acceptance.

Gateway admission (#849) treats wallet-funded, subscription-funded, cached and
explicit zero-priced requests alike for current key and organization authority,
mandate validity and concurrency. Monetary key/member/team/mandate caps keep
their wallet, bonus and credit-line posting basis; quota is not counted again.
An exact cap admits a zero-wallet request, while already-exceeded caps refuse.
Plan dunning restricts wallet overage only. The quota draw, monetary reservation,
durable watch and operation bind commit together. A refusal retains none of
those reservations. Independent identical bodies own distinct operations;
explicit `Idempotency-Key` claims retain the existing replay semantics. A free
request consumes neither wallet money nor quota and records a verifiable
zero-cost receipt when it settles.

## Basics

- Prefix: `/api/v1`. `/healthz` needs no auth and sits outside the prefix — so does `/readyz`, the readiness probe (200 only when the database answers and its schema is current, issue #619). `GET /api/v1/version` is unauthenticated inside the prefix: the deployment's own version/gitSha/schemaVersion identity. Also unauthenticated: `POST /api/v1/auth/register`, when the deployment allows signup at all, `POST /api/v1/invitations/redeem` — the invitation link's token is its credential — `POST /api/v1/webhooks/stripe`, where the `Stripe-Signature` header is the credential, `POST /api/v1/webhooks/airwallex`, where `x-timestamp` + `x-signature` are, and `POST /api/v1/webhooks/merchants/{merchantId}/versions/{versionId}`, where the rail's own signature header is the credential.
- Auth: two credentials, one principal. `Authorization: Bearer oxs-…` is an organization's API key; the `oxsum_session` cookie is a logged-in user's session (`POST /api/v1/auth/login`, `HttpOnly`, `SameSite=Lax`, `Path=/`, plus `Secure` when the deployment sets `OXSUM_SESSION_COOKIE_SECURE`). Both resolve to the organization the request acts for, so no endpoint takes a tenant from the caller; a session additionally carries the acting user and their role. Any explicit machine credential header (`Authorization`, `x-api-key`, `x-goog-api-key`), including an empty or malformed value, prevents cookie fallback. Header precedence is Authorization, then x-api-key, then x-goog-api-key. Unknown, revoked and expired credentials are all `UNAUTHORIZED`, with no hint which it was. The cookie is never accepted on `/v1`: the gateway takes an API key only, presented as `Authorization: Bearer`, as `x-api-key` — the spelling an Anthropic SDK sends — or as `x-goog-api-key`, the spelling a Gemini SDK sends. Conversely, the business surfaces that require a personal session refuse machine credentials outright: a request presenting any machine-credential header answers 403 `business authority requires a personal session`, and a request with no usable cookie answers 401 — an explicit key can never inherit ambient browser rights.
- OpenAPI credential declarations use defined scheme names. Alternatives are separate `security` objects: `sessionCookie`, `bearer`, `apiKeyHeader` and `googApiKeyHeader` describe the personal cookie and three consumption-key transports. Cookie and machine credentials are never required together. Own plan, journal, statement and order operations declare only `sessionCookie`; `/org/usage-report` additionally requires an owner or admin membership. Published plan/bundle catalogues and the existing scoped promo-lot read retain their consumption-key alternatives. `x-consumption-key-access: denied` records an operation's existing key refusal.
- Time: ISO 8601, UTC. The posting date is decided by the server.
- Money: integers in minor units, 1 credit = 1_000_000. Field names end in `Minor`.
- Field naming: camelCase for both requests and responses.
- Organization tenant ids: 32 lowercase hex characters, the organization's UUID without dashes. The ledger lives in the schema `ledger_<tenantId>`. Clients only ever read this value; they never send it.
- Idempotency keys: non-empty UTF-8 strings, at most 128 bytes.
- `/v1/*` is a second surface with the same key: a protocol-native gateway whose bodies and errors are the client's own — OpenAI's on `/v1/chat/completions`, `/v1/responses`, `/v1/embeddings` and `/v1/rerank`, Anthropic's on `/v1/messages`, and Gemini's `generateContent` pair under `/v1beta`. It is documented in its own section below.
- `/api/v1/metering/*` accepts owner-issued `oxs-int-…` integration credentials (`Authorization: Bearer`) with `meteringWrite` and the selected customer. Customer keys and personal sessions cannot meter. The separate `customersRead` scope grants only business customer list/detail.
- `/api/v1/admin/*` is a fourth: the platform admin's surface, opened by `OXSUM_ADMIN_TOKEN` rather than by an API key. It answers in the same envelope and the same error codes as the rest of `/api/v1`, and is documented in its own section below.
- `POST /add-checkpoint` is a fifth, off the prefix entirely like `/healthz`: the deployment's own C2SP `tlog-witness` endpoint (issue #260). It speaks `text/plain` on the way in and out — a foreign log's signed checkpoint plus consistency-proof lines in, the note with this witness's `cosignature/v1` line out — not the `/api/v1` envelope, because the submitting witness client files the answer verbatim as its cosigned note. It is only mounted when `OXSUM_WITNESS_SIGNING_KEY` and `OXSUM_WITNESS_TRUST` are configured, and is documented under "Tree heads" below.

## Idempotency

Every write to the ledger requires an `idempotencyKey` (holds, settlements, top-ups) — except the ones that derive their own: a settlement keys itself off its hold, and a redemption off the code (`redemption:<code_id>`), so neither takes the field. Writes that are not ledger entries — API keys, memberships, minted redemption-code batches — do not take one: creating a key twice creates two keys, and adding a member who is already one is `CONFLICT` rather than a replay.

- Replaying the same key with the same content returns the original receipt with `isNew: false`; nothing is booked twice.
- The same key with different content is `CONFLICT`; the original record is never overwritten. "Different content" means a different amount, a different kind of write, or any other field the entry is built from.

On timeout or network errors the client retries with the same key, never a new one.

## Holds and settlements

Customer `POST /api/v1/holds` and `POST /api/v1/settlements` are retired with 403 `FORBIDDEN` before body parsing or money/watch mutation (#625). `GET /api/v1/holds` is retained. The following invariants describe trusted gateway/recovery and service-metering operations.

- A hold reserves part of the available balance. The settled balance is untouched until a settlement discharges the hold; the difference between the amount held and the amount charged goes back to the available balance.
- A settlement names the hold it releases (`holdKey`): the server reads the hold's amount from the hold entry in the ledger, so there is no amount to assert. One hold settles at most once — the settlement entry's idempotency key is derived from the hold's key (`oxsum_core::settlement_key_for`), and the ledger's idempotency gate refuses a second settlement of the same hold with `CONFLICT`, inside the append. Retrying the identical settlement replays it.
- Naming a hold that is not outstanding is `NOT_FOUND`, even when other holds would cover the amount. The wallet's limit still refuses a release the outstanding reservations cannot cover, as the backstop behind the pairing.
- A hold taken with an API key is checked against the key's spend limit when it has one: settled charges plus outstanding holds attributed to the key may not exceed `spendLimitMinor`. With a `budgetDuration` the limit is periodic: `daily`, `weekly` and `monthly` bound the UTC calendar day, ISO week and month respectively — settled charges inside the current window count, plus every outstanding hold whatever its age, because it still reserves money now. The check is serialized per key and atomic with the ledger append — concurrent holds cannot together exceed it — and the refusal is 429 `KEY_LIMIT_EXCEEDED`.
- Two more per-key protections run beside the spend limit: `requestsPerMinute` caps how many requests the key may start inside a rolling minute — consumed at admission on the gateway, refused 429 `RATE_LIMITED` with `Retry-After` and `X-RateLimit-Limit`/`X-RateLimit-Remaining`/`X-RateLimit-Reset` headers — and `maxConcurrentHolds` caps how many holds the key may have outstanding at once, counted from monetary pending entries plus explicit zero-wallet reservations under the same per-key lock, refused 429 `TOO_MANY_HOLDS`. A replayed request is answered before any of these checks run: a retry never consumes the key's windows twice.

## Funding (deposits)

Money arriving is one row on `oxsum.deposits` per rail payment, whatever the rail — `manual`, `redemption`, `stripe`, `airwallex`, `hyperswitch`, `x402` and `solana` today. `POST /api/v1/topups` is retired: an authenticated customer receives 403 `FORBIDDEN` without a ledger write (#624). Customer payment checkout and redemption paths require a personal session and remain available during suspension.

`POST /api/v1/admin/organizations/{organizationId}/manual-receipts` takes `idempotencyKey`, positive `amountMinor`, `reference` and `reason` under deployment authority. The reference is deployment-wide unique; the idempotency key is customer-scoped. All terms must match on replay, otherwise 409. The intent and `manual_receipt.authorized` audit commit together before crediting; ledger key `manual-receipt:<receiptId>` allows recovery by repeating the identical request after interruption. The deposit carries that ledger key as `payment_ref`. Replay creates no extra receipt, deposit or audit event. This API uses existing credit units; merchant currency conversion remains a separate release task.

- `POST /api/v1/redemptions` — redeem an operator-minted code. Body `{"code"}`; answers the credit's `Receipt` plus `amountMinor` (the code does not name what it carries). The redeem is one atomic claim: the code's row is taken `FOR UPDATE`, its deposit is recorded `confirmed` and the ledger credits the purchased pool under `redemption:<code_id>`, so a retried redeem — the caller's or a second in-flight call — replays the same credit and one code credits once ever. Unknown, spent and expired codes are all `NOT_FOUND`, with no hint which; a code this same organization already redeemed replays its own receipt (`isNew: false`).
- `POST /api/v1/topups/stripe` — open a Stripe Checkout deposit (issue #201; a deployment without `OXSUM_STRIPE_*` answers 503). Body `{"idempotencyKey", "amountMinor"}`: the key is the deposit's `payment_ref` — the `(rail, organization, payment_ref)` UNIQUE anchor — and the answer is `{"checkoutUrl", "depositId"}`. The first call creates a Stripe Checkout Session whose `client_reference_id` is the deposit id, stores the session id and URL on the deposit's `meta`, and redirects the user's browser to `checkoutUrl`; a replayed key answers the stored URL without calling Stripe again, so there is never a second session per intent. Money moves only on the webhook, never here.
- `POST /api/v1/webhooks/stripe` — the provider's callback, no credential (`security: []`): the `Stripe-Signature` header *is* the authentication, verified as HMAC-SHA256 of `"{t}.{body}"` under `OXSUM_STRIPE_WEBHOOK_SECRET` in constant time. A missing or bad signature is 401; on a deployment without the rail the route itself 404s. A verified event is always answered 200 — anything else is Stripe's retry signal — and the acting event types are: `checkout.session.completed`, which finds the pending deposit by `meta.stripeSessionId`, marks it `confirmed` and credits the purchased pool in one transaction under the fixed ledger key `stripe:<session_id>` (a replayed or raced delivery credits once ever, then enqueues `deposit.credited`) — unless the session was opened in `subscription` mode by `POST /api/v1/org/subscription/checkout` (issue #496), in which case it binds the plan named in the session's `metadata.oxsum_plan_id` to the `client_reference_id` organization with `source` `stripe` and `stripeSubscriptionId` from `data.object.subscription`; `checkout.session.expired`, which walks the deposit to `expired`; `invoice.paid`, which rolls the bound subscription's period forward to the invoice line's `period.end` (`lines.data[0].period.end`) — the rail's own renewal signal; and `customer.subscription.deleted`, which cancels the binding that carries the deleted `stripeSubscriptionId`. `data.object.amount_total` is recorded as `received_minor`; a mismatch with `expected_minor` is drift for the reconciler, never auto-patched (decision D2).
- `POST /api/v1/topups/airwallex` — open an Airwallex hosted-payment-page deposit (issue #285; a deployment without `OXSUM_AIRWALLEX_*` answers 503). Body `{"idempotencyKey", "amountMinor"}`: the key is the deposit's `payment_ref`, and the answer is `{"checkoutUrl", "intentId", "clientSecret", "depositId"}`. The first call logs the credential pair in for a bearer token, creates a PaymentIntent whose `merchant_order_id` and `request_id` are the deposit id, stores the intent id, `client_secret` and hosted-page URL on the deposit's `meta`, and redirects the user's browser to `checkoutUrl`; a replayed key answers the stored URL and secret without calling Airwallex again, so there is never a second intent per deposit. Money moves only on the webhook, never here.
- `POST /api/v1/webhooks/airwallex` — the provider's callback, no credential (`security: []`): the `x-timestamp` + `x-signature` pair *is* the authentication, verified as hex HMAC-SHA256 of `timestamp + body` under `OXSUM_AIRWALLEX_WEBHOOK_SECRET` in constant time. A missing or bad signature is 401; on a deployment without the rail the route itself 404s. A verified event is always answered 200 — anything else is Airwallex's retry signal — and two event names act: `payment_intent.succeeded` finds the pending deposit by `meta.airwallexIntentId`, marks it `confirmed` and credits the purchased pool in one transaction under the fixed ledger key `airwallex:<intent_id>` (a replayed or raced delivery credits once ever, then enqueues `deposit.credited`); `payment_intent.cancelled` and `payment_intent.expired` walk the deposit to `expired`. `data.object.amount` (decimal major units) converts to cents for `received_minor`; a mismatch with `expected_minor` is drift for the reconciler, never auto-patched (decision D2).
- `POST /api/v1/topups/hyperswitch` — open a Hyperswitch deposit (issue #319; a deployment without `OXSUM_HYPERSWITCH_*` answers 503). Body `{"idempotencyKey", "amountMinor"}`: the key is the deposit's `payment_ref`, and the answer is `{"paymentId", "clientSecret", "publishableKey", "depositId", "sdkUrl"}`. The first call opens a `pending` deposit, creates a switch payment (`confirm: false`, automatic capture, metadata naming the deposit, `return_url` rooted on `OXSUM_PUBLIC_URL`) and stores the payment id and `client_secret` on the deposit's `meta`; a replayed key answers the stored handles without a second payment. `sdkUrl` (`OXSUM_HYPERSWITCH_SDK_URL`, issue #320) is the origin the dashboard loads `HyperLoader.js` from — the API and the SDK bundle are different services, so the response carries both origins. The browser confirms with the SDK; money moves only on the webhook.
- `POST /api/v1/webhooks/hyperswitch` — the switch's callback, no credential (`security: []`): `X-Webhook-Signature-512` is hex HMAC-SHA512 over the raw body under `OXSUM_HYPERSWITCH_WEBHOOK_SECRET`, compared in constant time. A missing or bad signature is 401; on a deployment without the rail the route 404s. Every verified event is recorded in `oxsum.payment_events` under its `event_id` before it acts — a replay is acknowledged and does nothing twice. `payment_succeeded` finds the pending deposit by `meta.hyperswitchPaymentId`, marks it `confirmed` and credits the purchased pool in one transaction under the fixed ledger key `hyperswitch:<payment_id>` (then enqueues `deposit.credited`); `payment_cancelled` and `payment_expired` walk the deposit to `expired`. `content.object.amount` is recorded as `received_minor`; a mismatch with `amount_minor` is drift for the reconciler, never auto-patched (decision D2). Every other event type is recorded and acknowledged.
- `POST /api/v1/webhooks/merchants/{merchantId}/versions/{versionId}` — the scoped merchant callback (issue #649): the path coordinates select the merchant and the exact signing version *before* the raw body is verified, so a payload can never pick its own secret. A verified event is durably stored in `oxsum.merchant_event_inbox` scoped by `(merchantId, eventId)` across every retained version — replaying the same delivery is acknowledged once, and reusing an event id with different signed bytes is `409 CONFLICT`. A recognized payment event must match the pinned expected payment (`merchant_payment_bindings`) on merchant, version, customer, provider account, mode, amount and currency before it is `accepted`; a payment the store does not know stays `pending` for recovery rather than erroring. Receipt never credits a wallet directly — the legacy adapter applies an `accepted` `legacyDeposit` success idempotently, and refund/dispute events are authenticated and retained for their own settle path. A mismatched event is `rejected`; an `unsupported` one is `ignored`; both are acknowledged 200 so the rail stops retrying.
- `GET /api/v1/business/merchants/{merchantId}/events` — the redacted durable intake, behind the business `reportsRead` capability (owner/admin/finance/viewer). Ordered by receipt UUID, paged by `after`/`limit` (1–100). It returns only normalized coordinates — event id, kind, local `paymentId`, `state` (`pending`/`accepted`/`applied`/`rejected`/`ignored`) and rejection `reason` — never the raw provider payload or any credential.
- `POST /api/v1/orders` — pin a payment order (issue #606): the customer names the quoted `displayAmountMinor`/`displayCurrency`, the `purpose` (`walletTopup`, `bundle`, `planPeriod`, `invoiceRepayment`), its `purposeRef` (the plan a `planPeriod` activates, the receivable an `invoiceRepayment` settles), a bundle version in `purposeRef` (with deprecated `promoMinor` only a zero/absent or exact-offer assertion), and the `idempotencyKey` that anchors it. Optional `returnUrl`/`cancelUrl` (issue #607) name where the provider returns the browser — an absolute `http(s)` URL or a site-absolute `/` path resolved under the deployment's public URL; absent, the order's own `/app/orders?order={orderId}` page. `amountMinor`/`rate` are deliberately not request fields — the server pins bundle quantities to the published offer, plan-period fees to the customer's sold quote, and other conversions to the persisted Business collection policy before any provider call, so a client-supplied figure is a schema rejection, not a negotiated term. The answer is `{order, providerPaymentId, clientSecret, checkoutUrl, publishableKey, sdkUrl}` — a checkout to complete, never a paid order: `paid` is established only by the signed versioned webhook, and a replayed `idempotencyKey` returns the standing order and the same checkout handles the first attempt minted rather than a second charge. Changed purpose, amount, currency, reference or plan-quote fingerprint under a reused key is `409 CONFLICT`. For bundles, omitted/zero `promoMinor` leaves the offer authoritative on both create and replay; a nonzero assertion must match the original grant, including after offer retirement. Personal session required, like every checkout path.
- `GET /api/v1/bundles` — the authenticated customer catalog of published, non-retired offer versions. Fields are safe commercial terms only.
- `GET /api/v1/business/bundles` — current staff overview authority reads complete publication/retirement history. `POST` publishes `{idempotencyKey, reason, bundleId, expectedVersion, terms}` under current `commercialWrite` authority (owner/finance). New offers use null `bundleId` and revision zero; later versions name the stable offer and latest observed revision. The returned `{id, offer}` is an immutable audit receipt; unchanged retries retain that original receipt after retirement, while changed intent or stale revision conflicts.
- `POST /api/v1/business/bundles/{bundleVersionId}/retire` — reasoned, idempotent retirement under the same commercial authority. Existing orders keep their published snapshot. New bundle orders must name an eligible version, exactly match its fiat price/currency and cannot choose either grant quantity. Missing authority on legacy pending bundles fences both checkout recovery and fulfillment. Promotion expiry is anchored to `paidAt`; refund review starts only after bundle fulfillment, and confirmed partial/full refunds cancel remaining promotion without cash reimbursement.
- `GET /api/v1/orders` — the caller's own orders, newest-first page by `after`/`limit` cursor (1–100), with `purpose` and `status` filters (issue #607) — provider orders and staff-recorded manual receipts in the same list, told apart by `source`. An order of another organization is never listed.
- `GET /api/v1/orders/{orderId}` — one order the caller owns; another organization's id is `NOT_FOUND`. `status` is the lifecycle the signed webhook established — `created`, `providerPending`, `paid`, `fulfilled`, `expired`, `failed` or the `needsAttention` discrepancy a paid-but-mismatched event parked — and this read can never itself mark an order paid.
- `GET /api/v1/orders/{orderId}/fulfillment` — the order's state plus its `fulfillment` record once the grant lands: `{orderId, status, paidAt, fulfillment}`. `paid` with a null `fulfillment` is the honest mid-state the recovery sweep drains; the record names exactly what the purpose's grant produced (the wallet entry a top-up booked, the plan period activated, the receivable repaid).
- `GET /api/v1/orders/{orderId}/checkout-session` — the live checkout session the returned or refreshed browser polls (issue #607): `{status, providerPaymentId, clientSecret, checkoutUrl, publishableKey, sdkUrl}` — the stored provider handles plus the order's lifecycle truth, never a paid promise. A still-`created` order re-issues its provider call under the same order-UUID idempotency, so a page that lost the first answer's handles recovers the standing checkout rather than charging twice; `paid`/`fulfilled`/`failed`/`expired` answer their truth with whatever handles were stored, and a `manual` order's publishable/sdk fields are `null` — there is no checkout to mount.
- `GET /api/v1/business/orders` — the operator's cross-organization order list (issue #607), behind the business `reportsRead` capability: every customer's orders, newest first, with `organizationId`, `purpose` and `status` filters and the same `after`/`limit` paging.
- `POST /api/v1/business/orders` — record a verified out-of-band receipt as a manual order (issue #607), behind the business `fundsWrite` capability and a personal session: `{organizationId, idempotencyKey, displayAmountMinor, displayCurrency, reference, reason}` lands a `source: manual` `walletTopup` order that is `paid` on the receipt's own `evidence` and fulfills through the same grant path a webhook-paid order takes — the wallet cash is never written by hand. `reference` is the receipt's unique evidence per business, so one receipt can never fund twice; a replayed `idempotencyKey` returns the standing order, a reused `reference` conflicts `409`.
- `GET /api/v1/business/payment-reconciliation` — the paid/fulfilled/`needsAttention` reconciliation, behind the business `reportsRead` capability: per-state counts plus `paidUnfulfilled`, the order ids the recovery sweep still owes a fulfillment. `paid − fulfilled` should drain to zero; `needsAttention` never auto-resolves.
- `GET /api/v1/business/financial-facts` — the financial figures (issue #613), behind `reportsRead` and a personal session: `{usageMinor, ratedMinor, entitlementMinor, fundingMinor, billedMinor, earnedMinor, deferredMinor, collectedMinor, refundMinor, discountMinor, receivableMinor}` derived from the source rows (`usage_records`, `invoice_journals`, `payment_orders`, `plan_statements`, refunds, draws — never a snapshot). Filters: `organizationId`, `model`, `channel`, `from`/`to` (UTC `YYYY-MM-DD`), `timeBasis` (`booking` the settle day, `service` the ledger `booking_date`, `payment` the money tables' own stamps).
- `GET /api/v1/org/usage-report` — the customer's own projection of the same financial figures, scoped to the caller's organization (`AccountManage`): the same read the staff surface runs, so the customer total and the business sum for that organization agree by construction. Same `from`/`to`/`timeBasis` filters.
- `GET /api/v1/business/cost-coverage` — the `cost_basis` breakdown (`reportsRead`): per-bucket `{turns, costMinor, untrackedTurns}` for `realAttempt`/`cacheReuse`/`estimate`/`missing`/`providerReconciled`, `failedAttempts` (`errored`/`refused` separated so a failed-but-billed upstream charge is never mistaken for a clean zero), and `byProviderDay` at the `(serviceDate, channel, model)` grain the upstream-bill diff reads. `missing`/`NULL` basis turns are the coverage gap — cost was never tracked, which is not the same as cost zero.
- `GET /api/v1/business/reconciliation-runs` — the run history (`reportsRead`), newest first, `after`/`limit` paging. Each run is one pass over a period's books: `kind` `daily|periodClose|manual`, `state` `running|matched|exceptions|failed`, the frozen `expectedTotals`/`actualTotals`, `exceptionCount`, `lastError` on a failed pass.
- `POST /api/v1/business/reconciliation-runs` — a staff-requested `manual` run over `{periodStart, periodEnd}` under `fundsWrite` + `finance_audit`: idempotent under `(kind, period)` — a replayed `idempotencyKey` re-runs under the run's own identity rather than minting a second history.
- `GET /api/v1/business/reconciliation-runs/{runId}` — one run; a missing id is `NOT_FOUND`.
- `GET /api/v1/business/reconciliation-runs/{runId}/exceptions` — a run's exceptions (`reportsRead`); `?open=true` narrows to the standing work list.
- `GET /api/v1/business/reconciliation-exceptions` — every exception, `?open=true` by default: `{exceptionId, runId, organizationId, kind (unmatchedUsage|unmatchedJournal|unmatchedPayment|amountDrift|missingCost|doubleEntry), expectedMinor, actualMinor, sourceTable, sourceRef, detail, state (open|resolved|waived), resolution}`.
- `POST /api/v1/business/reconciliation-exceptions/{exceptionId}/resolve` — the explicit close under `fundsWrite` + `finance_audit` (`reconciliation.resolve`): `{idempotencyKey, resolution}` names the corrective outcome. A non-open exception is `NOT_FOUND` — a close is never silent.
- `POST /api/v1/business/reconciliation-exceptions/{exceptionId}/waive` — close `waived` under the same capability/audit (`reconciliation.waive`): `resolution` says why the drift is not a defect.
- `POST /api/v1/business/reconciliation-exceptions/{exceptionId}/repairs` — the corrective action under `fundsWrite` + `finance_audit` + an immutable `reconciliation_repairs` row (`before`/`after` snapshots, `auditRef`): `{idempotencyKey, action, reason, patch}`. `reclassifyCost` rewrites the usage row's `costBasis`/`upstreamCostMinor` under lock; `adjustJournal`, `linkPayment`, `markDuplicate` record the audited instruction — the next run's re-check is the close, never a balance overwrite.
- `GET /api/v1/business/audit` — the `finance_audit` ledger the staff console's audit page reads (issue #615), behind `reportsRead`: every immutable resolution row `{auditId, actorUserId, action, target, detail, noteId, createdAt}`, newest first, `after`/`limit` keyset paging and an optional `action` filter (`refund.approve|decline|release`, `dispute.resolve`, `journal.void`, `statement.release`, `reconciliation.resolve|waive|repair`).
- `POST /api/v1/orders/{orderId}/refund-request` — ask for a refund review on one of the caller's own orders (issue #608): body `{idempotencyKey, amountMinor?, reason}`. `amountMinor` is the credit asked back; absent names the refundable remainder. A replayed `idempotencyKey` answers the standing refund (`requested`); changed terms under a reused key are `409`. The amount may not exceed the order's funded `amountMinor` minus what live refunds already reserve or returned — `VALIDATION_ERROR` over the bound, never a negative wallet. Personal session required — the request is a review, never a grant.
- `GET /api/v1/orders/{orderId}/refunds` — the caller's refunds on one order plus `refundableMinor`, what it may still ask back.
- `GET /api/v1/business/refunds` — the staff finance list behind `reportsRead`, newest page by `after`/`limit` cursor.
- `GET /api/v1/business/refunds/{refundId}` — one refund the business owns; another business's id is `NOT_FOUND`.
- `POST /api/v1/business/refunds/{refundId}/resolve` — approve or decline a `requested` refund under `refundsWrite`: body `{idempotencyKey, decision: approve|decline, reason?}`. Approve commits the durable `provider_pending` reservation plus an `issued` credit note in one transaction *before* the PSP call — `refundId` is the provider's idempotency key — then issues the refund and answers the standing `provider_pending` row, never a promise the provider refunded. A replayed resolve answers the standing reservation without a second provider call. The signed `refund_succeeded`/`refund_failed` webhook supplies actual provider facts: success records complete Cash against original source or review suspense and applies an issued note, while failure releases the reservation and reverses an issued note. Non-plan actual cash remains visible in `cashEffect` beside an unchanged late terminal state, consumes the refundable bound and fences further payout authorization; unknown outcomes retain the reservation after the five-attempt recovery budget, with `nextRetryAt: null` signaling reconciliation. Only a proven first-attempt pre-creation refusal may release without a terminal event. `decline` lands `declined` with audit.
- `GET /api/v1/business/credit-notes` — the order-linked corrections the refund flow issued (`reportsRead`), `state` walking `issued → applied` as the take-back books, `reversed` on a released refund.
- `GET /api/v1/business/disputes` — contested charges only, never refunds (`reportsRead`): `state` `opened → evidenceDue → won|lost|closed`.
- `POST /api/v1/business/disputes/{disputeId}/resolve` — resolve a dispute under live `refundsWrite`: body `{idempotencyKey, resolution: evidenceDue|won|lost|closed, reason?}`. Authority, exact customer-wallet scope, any ledger/documents, selected state and audit share one transaction. Same-key replay requires the same actor, target, resolution and trimmed optional reason; changed terms conflict. A different terminal outcome or inconsistent retained source/audit returns `409`. Same-terminal-outcome requests with an unused key return the standing row without a new audit. Plan losses record complete cash against original receivables/payable or review suspense. New non-plan losses credit complete Cash, debit only proven retained original Wallet funding, and place the rest in explicit review suspense. Finance audit includes `walletTakenMinor`, `unallocatedMinor`, `requiresReview`, `source` and `cancelledPromoLotId`; `source.policy=nonplan-cash-v1` retains original entry/hash, fulfillment, source Wallet/repayment split, later settled debits, pending reservations and prior actual chargeback cash. New refunds/approval and unperformed grants are fenced after loss. Hyperswitch intake reads the actual `dispute_id` field (an unambiguous legacy `refund_id` is compatible); contradictory supplied identities are malformed. All order-dispute intake, including plan payments, requires a case ID and exact pinned currency; partial fiat amounts must convert to integral credit minor units. Duplicate case notifications preserve the original amount and do not increment the journal contest counter again; conflicting original terms are rejected. `OrderDispute.amountMinor` is credit minor units, while `currency` names the original fiat currency. Historical wrong-direction effects use the [exact local #842 correction](nonplan-chargeback-recovery.md); non-plan refund recovery uses its separate exact local #841 workflow. [Historical source review](dispute-resolution-review.md).
- `POST /api/v1/topups/x402` — credit a signed payment authorization (issues #264, #282; a deployment without the rail configured answers 503). Body `{"payment"}` — base64url of the authorization `{scheme, network, asset, amountMinor, payTo, nonce, payer, signature}` — plus an optional `amountMinor` bound pinning what the verified authorization may carry. The rail's verifier is the seam (`RailVerifier` trait) with two legs, verify then settle: under the real rail (`OXSUM_X402_FACILITATOR_*` — a full four-set or none) both legs delegate to an x402-rs facilitator sidecar speaking the V1 wire, the authorization's `nonce` anchors `payment_ref`, and the settled chain transaction hash lands on the deposit's `facilitator_tx_hash` — a replayed authorization replays the credit (`isNew: false`) and never settles twice (the deposit row is held `FOR UPDATE` across the settle call, so a same-nonce race serializes and the loser skips the chain). The stand-in (`OXSUM_X402_VERIFYING_KEY`, used only when no facilitator is configured) checks an Ed25519 test-scheme signature over the canonical body and settles trivially. Anything the verifier does not honor — bad base64, bad JSON, a foreign scheme/network/asset, a signature that does not check, a settle the chain refuses — is `FORBIDDEN`, indistinguishably. A refused settle leaves the deposit `confirmed` with the facilitator's `error_reason` (or a local transport note) recorded on `meta.settleRefusal`, so an operator reads *why* the rail would not land it without the sidecar's logs; a retry under the same nonce replays the claim and re-attempts the settle. Answers the credit's `Receipt`.
- `POST /api/v1/topups/solana` — open a Solana top-up intent (issue #1117; a deployment without `OXSUM_SOLANA_*` answers 503). Body `{"amountMinor"}` — the credit to book; the answer is `{reference, payTo, amountLamports, splToken, expiresAt}` — `reference` is the per-intent marker account the paying transaction must carry, `payTo` the deposit address, `amountLamports` the lamports (or SPL base units when `splToken` names a mint) the payment must deliver at the intent's locked rate. The rail verifies the transfer by the Solana Pay recipe — locate the transaction carrying `reference`, then prove it pays `payTo` at least `amountLamports` of the right asset — over `OXSUM_SOLANA_RPC_URL`; `OXSUM_SOLANA_RATE_MICRO` (minor-credits per whole SOL) is the intent's locked rate, the devnet stand-in a mainnet oracle replaces. `payment_ref` anchors on `reference`, so one intent opens one deposit ever; a `pending` intent lapses at `expiresAt` (`OXSUM_SOLANA_INTENT_TTL_SECS`, default 600).
- `POST /api/v1/topups/solana/confirm` — settle an intent now rather than waiting for the `solana-deposits` poll tick. Body `{"reference"}` — the intent's marker, not the chain signature: the rail locates and validates the paying transaction itself, so the caller never names what it paid. `403` means no honoring transfer has landed yet, or the found one does not honor the intent (wrong recipient, underpayment, wrong mint, missing marker). The located signature lands in `facilitator_tx_hash` and keys the `solana:<signature>` ledger credit, so a raced poll or a replayed confirm credits nothing twice. Answers the credit's `Receipt`.

## Statements

All statement/report/share-link APIs require a current owner/admin customer session. Consumption keys and member sessions receive 403. Public share links remain explicitly issued, expiring bearer capabilities.

An organization that draws its credit line settles monthly (issue #124): the platform issues one statement per organization and UTC `YYYY-MM` period, itemized by channel and model, and repayments settle the oldest open statement first.

- `GET /api/v1/statements` — the organization's finalized statements, newest period first: `period`, `totalMinor` (the month's charges), `creditDrawnMinor` (what the line carried — what the statement is owed), `paidMinor`, `outstandingMinor`, `paymentStatus` (`pending`, `paid`, `overdue`, `suspended`), the `paymentTermsDays` snapshotted at issue, `dueDate`, and the ledger window (`logFromIndex`/`logToIndex`) the statement's lines prove. Drafts are the platform's working documents and never appear here.
- `GET /api/v1/statements/{statementId}` — one finalized statement with its `lines` (channel, model, turns, token totals, `amountMinor`) and its `anchor` (issue #221): `{entryId, logIndex, windowHash, rollupHash, origin, note, keyName, publicKey, keyHash}` — the marker entry the finalization appended to the organization's ledger, the window fold over the covered entries' `contentHash` bytes in log order, the rollup digest over the statement's figures and lines, and on a signing deployment (`OXSUM_HEAD_SIGNING_KEY`) the operator's signed note over the tree head right after the marker; unsigned, the four key fields answer `null` and the anchor still binds both digests. A draft or another organization's statement is `NOT_FOUND` — an id probes nothing. Verify it client-side with `oxsum_verify::verify_statement_anchor`.
- `GET /api/v1/statements/{statementId}/proof` — the self-contained proof bundle (issue #479): the detail's `{statement, lines, anchor}` plus the three inputs `verify_statement_anchor` cannot read out of the document itself — `windowHashes` (the covered entries' `contentHash` hex strings in log order, to refold under `oxsum/statement-window/v1` rather than trust `windowHash`), `expectedHead` (the signed tree head at `anchor.logIndex + 1`, `null` on a deployment that signs no heads — the same honesty `anchor.note` keeps) and `expectedOrigin` (`oxsum/ledgers/<tenant>`). The point of the endpoint is that the verifier runs off the answer alone — no database read, no `/log/head` fetch, no stitching entries together. A draft, a foreign or a missing id is `NOT_FOUND`, the same guard the detail keeps.
- `GET /api/v1/statements/{statementId}/export.pdf` — the same finalized statement rendered as a PDF (issue #205): header (organization, period, status, due date), the itemized lines, the totals, and a footer anchoring the page to `logFromIndex`/`logToIndex` beside a QR back to `/verify`. The body is binary `application/pdf`, not the JSON envelope; a draft, a foreign or a missing id is `NOT_FOUND` — the same guard the detail endpoint keeps. The export always renders the stored lines, so the document can never disagree with the detail answer.

- `POST /api/v1/org/usage-reports` — mint a signed usage report for one `endUser` over `[from, to]` (issue #261), or replay the standing one. An organization bills its downstream customers out of a ledger that mixes every customer's usage, so a customer cannot verify a bill by reading the log — a usage report is the bundle the org hands one `endUser`: the settlement entries attributed to them over the span, each with its own inclusion proof, plus the marker entry the org committed into the ledger once. The marker is a self-balancing 1-minor `Equity:Adjustments` entry whose `DocumentRef` binds the subset digest (`oxsum/usage-report/v1`, the fold of the shown entries' content hashes in log order) and whose description commits `{endUser, from, to, entryCount, chargedTotalMinor, subsetHash}`. One marker per `(organization, endUser, from, to)`, ever: the idempotency key derives from the span, so a re-request replays the standing report rather than committing a second subset — the once-per-span rule the completeness claim rests on. Both dates are inclusive UTC `YYYY-MM-DD` and `to` must be a fully ended day; an open day or an `endUser` the org has no usage for is `VALIDATION_ERROR`. The answer is the `UsageReport` bundle — `reportId` (the marker's own id, so `GET /api/v1/entries/{reportId}/proof` resolves it), `endUser`, `from`, `to`, `entryCount`, `chargedTotalMinor`, `subsetHash`, `marker` (`{entry, contentHash, proof}`), `entries` (each shown settlement `{entry, contentHash, proof}`), `head`, and on a signing deployment `note`/`keyName`/`publicKey`/`keyHash`. Verify it client-side with `oxsum_verify::verify_usage_report`. Completeness is the org's signed claim, not a Merkle fact — the marker attests "these are all the settlements for `endUser` over the span", and a customer who kept their own request log detects a dropped row as a count or total mismatch; the bundle cannot enumerate rows the org chose never to show.
- `GET /api/v1/org/usage-reports` — every report the organization has minted, newest first: the descriptors alone (`reportId`, `endUser`, `from`, `to`, `entryCount`, `chargedTotalMinor`, `subsetHash`), no rows or proofs — a mint is rare, so the list is enough to pick the report whose bundle the detail endpoint serves.
- `GET /api/v1/org/usage-reports/{reportId}` — the frozen bundle: `reportId` is the marker entry's own id. The committed subset is frozen at mint (`usage_report_rows`), so re-fetching serves the identical descriptor and rows, never a re-query — a report cannot grow to absorb usage the org omitted the first time. An id under another organization is `NOT_FOUND`: a report answers only for the org that minted it.
- `POST /api/v1/org/usage-reports/{reportId}/links` — mint a share link to one report (issue #300). A downstream customer holds no account, so the link is the capability: the plaintext `token` is answered once, the database keeps only its SHA-256, and the deployment's `/bill/{token}` page resolves it to the frozen bundle without a session. `expiresAt` defaults to 30 days from now and may not be more than 90 days out; `label` is the organization's own note for the recipient. A report of another organization is `NOT_FOUND`. Either mint also accepts an optional `email` (issue #315): the link is mailed at creation — the only moment the plaintext exists — and `delivery` answers `sent` or `failed`; a deployment without a mailer refuses the address with `VALIDATION_ERROR` rather than minting a link nobody receives.
- `POST /api/v1/org/usage-reports/links` — the `end_user` scope: one link that lists every report the organization has issued one downstream customer. The customer must have usage in the organization's ledger, else `VALIDATION_ERROR` — a link to a name nothing was billed to would open an empty page.
- `GET /api/v1/org/usage-reports/links` — every link the organization minted, newest first: the descriptors with `createdAt`, `expiresAt`, `revokedAt` and `lastUsedAt` (the operator's signal that the customer opened the bill). Tokens are never part of the answer.
- `DELETE /api/v1/org/usage-reports/links/{linkId}` — revoke a link, idempotently: revoking an already-revoked link answers the same descriptor, and an id of another organization is `NOT_FOUND`.

## Evidence bundles

A usage report commits a *claimed subset* — the settlements the organization says are all of one `endUser`'s span. An evidence bundle (issue #616) goes further: a version-2 export that commits the whole closed billing period — usage records, plan statements, invoice journals, payment orders and their fulfillments, refunds, disputes, credit notes, and the day-seal/anchor digests — frozen at mint into `bundle_json` byte-for-byte, so re-downloads serve the identical document the ledger committed. The commit is the same marker shape the statement anchor and usage report use: a self-balancing 1-minor `Equity:Adjustments` entry whose `DocumentRef` binds the manifest digest (`oxsum/evidence-bundle/v1`), idempotency-keyed once per scope — a re-mint of the same span replays the standing bundle, never commits a second version of the same period. The manifest names every artifact's `kind`/`id`/digest; the verifier re-digests each served artifact under `oxsum/evidence-artifact/v1/<kind>`, refolds the manifest, checks the marker binds it, adopt-verifies each committed ledger entry into the signed head, and verifies the head's note under the published key. The bundle's `scope` block is the honest half: `covered` names the committed classes (plural: `usageRecords`, `planStatements`, `invoiceJournals`, `paymentOrders`, `refunds`, `disputes`, `creditNotes`, `daySeals`, `ledgerEntries`), `excluded` names what the bundle does not answer for (other organizations' rows, history outside the span, in-flight operations), and `upstreamCostIncluded` records whether the platform's own supplier-cost figures exported — `upstreamCostMinor`/`real_attempt` are the platform's cost ledger, not the customer's bill, so the default mint redacts them and `OXSUM_EVIDENCE_UPSTREAM_COST` opts back in; `costBasis` always exports. Verify it client-side with `oxsum_verify::verify_evidence_bundle` — the npm package's `verifyEvidenceBundle`.

All six surfaces are Owner/Admin-session only (`x-customer-role-access: owner-admin`) — a bundle exports the organization's entire billing span, the same tier the usage-report mint sits at — except the public token download, which carries no session at all.

- `POST /api/v1/org/evidence-bundles` — mint the period bundle for `[from, to]` (inclusive UTC `YYYY-MM-DD` days, `to` must be fully ended), or replay the standing one for that span. The answer is the `EvidenceBundleSummary` descriptor — `bundleId`, `kind` (`period`), `periodFrom`/`periodTo`, `manifestDigest`, `anchorLogIndex`, `entryCount`, `createdAt` — never the bundle bytes, which the download serves.
- `GET /api/v1/org/evidence-bundles` — every bundle the organization minted, newest first: the descriptors alone.
- `GET /api/v1/org/evidence-bundles/{bundleId}` — the frozen `EvidenceBundle` bytes under `data`. The stored `bundle_json` is served verbatim — the manifest's digests committed mint-time serialization, so the endpoint wraps the stored bytes in the envelope rather than re-serializing. An id under another organization is `NOT_FOUND`.
- `GET /api/v1/org/statements/{statementId}/evidence` — the statement-scoped bundle (`kind` `statement`): minted on first ask for that one plan statement and replayed after — the single-document export a dispute or an audit asks for. A draft, a foreign or a missing statement id is `NOT_FOUND`.
- `POST /api/v1/org/evidence-bundles/{bundleId}/share` — mint a share token for the bundle. Body `{label?, expiresAt?, maxDownloads?}`, every field optional: `expiresAt` is the token's deadline, `maxDownloads` caps how many times it resolves. The answer is `{share, token}` — the plaintext `oxe-…` token is served exactly once; the database keeps only its SHA-256.
- `GET /api/v1/org/evidence-bundles/{bundleId}/share` — the bundle's shares, newest first: `shareId`, `label`, `createdAt`, `expiresAt`, `maxDownloads`, `downloadCount`, `lastDownloadAt`, `revokedAt` — the audit a "who fetched this" question reads. Tokens are never part of the answer. A bundle of another organization is `NOT_FOUND`.
- `DELETE /api/v1/org/evidence-bundles/{bundleId}/share` — revoke every live share on the bundle; the answer `{"revoked": n}` counts what the call stamped. Idempotent — a second delete answers `revoked: 0`.
- `GET /api/v1/evidence/{token}` — the public download (`security: []`): the token is the credential, resolving without a session to the same verbatim `{"data": <bundle>}` the owner download serves. Expired, revoked, exhausted and never-existed tokens all answer the same `410` — a guessed token leaks nothing about which shares exist — and each successful download spends one of the token's budget atomically (`download_count < max_downloads` in the resolving UPDATE), so concurrent fetches cannot overspend a cap.

Payments are not something the organization makes against a statement directly: any top-up or redemption repays the drawn credit line first, and the reconciliation that follows every money-in applies it to the oldest open statement — so paying off a bill is just funding the wallet.

## Subscriptions

A subscription plan grants a per-period credit quota (issue #496) — entitlement, not ledger money: quota spends never move a ledger account, they run a counter on `oxsum.org_subscriptions.consumed_minor` against the plan's `quotaMinor` snapshotted at bind time. A gateway request draws from the active subscription first, up to the freeze, and the ledger holds only what the draw did not cover; at settlement the drawn share is recorded on `oxsum.subscription_draws` (keyed by the hold's `hold_key`, so a replayed request settles once) and the subscription block on the settlement's v9 description binds `{subscriptionId, periodId, drawnMinor, chargedMinor}` into the charge's content hash — a verified bill proves what the period paid. What the plan's `allowWalletOverflow` does with an exhausted quota: `true` holds the remainder against the wallet, `false` refuses the request (`402 PAYMENT_REQUIRED`). A `resetPeriod` boundary resets the counter — `daily` (next UTC midnight), `weekly` (next Monday 00:00 UTC), `monthly` (the first of next month) — by closing the open period `boundary` and chaining a fresh one from the boundary it crossed, lazily on the first draw after it and hourly from the `subscription-rollovers` leader job, so a quiet gap closes every period it spanned rather than inventing one window from now. `never` plans carry no `currentPeriodEnd`: the counter runs until a cancel or the rail's `deleted` event ends the binding — nothing rolls, nothing expires. Every transition writes a `oxsum.subscription_periods` row whose `outcome` (`open`, `boundary`, `cancelled`, `expired`, `upgrade`, `downgrade`) is the audit trail.

- `GET /api/v1/org/subscription` — the organization's active subscription, or `null` while none is bound: `{subscriptionId, organizationId, planId, state, currentPeriodStart, currentPeriodEnd, consumedMinor, quotaMinor, allowWalletOverflow, source, stripeSubscriptionId, createdAt}`. `consumedMinor` is this period's draw against the snapshotted `quotaMinor` (null quotas meter without a cap — every request draws in full); `currentPeriodEnd` is null on a `never` plan. Session or API key, like every `/api/v1/org/*` read.
- `POST /api/v1/org/subscription/checkout` — buy a plan through Stripe: `{"planId"}` opens a Checkout session in `subscription` mode for the plan's `stripePriceId`, answered as `{"checkoutUrl"}` — the browser pays there and the webhook binds the plan (a `checkout.session.completed` carrying `data.object.subscription`); nothing meters until that event lands. A plan without a `stripePriceId` is admin-bound only (`VALIDATION_ERROR`); a deployment without `OXSUM_STRIPE_*` answers 503, like the deposit rail.
- `GET /api/v1/org/subscription/periods` — the binding's period history, open period first: `{periodId, subscriptionId, organizationId, periodStart, periodEnd, quotaMinor, consumedMinor, outcome, createdAt}` — the record every state transition left, each closed period's `consumedMinor` frozen at what it actually drew.
- `GET /api/v1/org/promo-lots` — the organization's promotional lots (issue #609), soonest-expiring first — the order admission draws them in: `{lotId, organizationId, source, sourceRef, amountMinor, remainingMinor, reservedMinor, expiresAt, createdAt, closedAt, closeReason}` each, where `remainingMinor` is what new holds can still pin and `reservedMinor` what open holds already named. `?includeClosed=true` keeps `expired`/`cancelled` rows in the answer. Session or API key — a lot is the organization's own granted credit, the same visibility the balance carries; the money a lot holds draws before every other pool, and expired promo burns rather than refunds.

## Webhooks

An organization registers HTTPS endpoints and oxsum POSTs a signed event to each one when money moves (issue #144). Endpoints live under the same credential check as the rest of the organization surface.

- `POST /api/v1/webhooks` — register an endpoint: `{"url", "events", "balanceLowMinor"?}`. `url` must be HTTPS, or HTTP only to localhost; `events` names the catalog entries it subscribes to — `request.settled`, `org.suspended`, `deposit.credited`, `deposit.failed`, `deposit.reversed`, `deposit.disputed`, `balance.low`, `statement.finalized`, `statement.payment_overdue`, `statement.payment_status_updated`, `org.credit_limit_exceeded`, `anomaly.detected`, `budget.exhaustion_forecast`, `budget.threshold_crossed`, `task.succeeded` and `task.failed`. `balanceLowMinor` arms `balance.low`: the event fires once when a settlement drops available balance under the line, and again only after a deposit lifts it back over. `budgetThresholdsPercent` arms `budget.threshold_crossed`: an integer list like `[50, 80, 95]`, each entry a consumption percentage of the scopes `GET /api/v1/budget` reads (issue #232); absent or empty is off. The answer carries `secret` — `whsec-` plus 48 hex — exactly once; afterwards only `secretLast4` is readable. `org.suspended` carries `{organizationId}` in `data` and fires once per suspension transition — a replayed flag enqueues nothing, and reinstatement sends no event.
- `GET /api/v1/webhooks` — the organization's endpoints, newest first, never the secret.
- `DELETE /api/v1/webhooks/{endpointId}` — removes the endpoint; its queued deliveries go with it (the endpoint stops being called). Another organization's id is `NOT_FOUND`, not `FORBIDDEN`.
- `GET /api/v1/webhooks/{endpointId}/deliveries` — the 50 most recent delivery records for the endpoint: `status` (`pending`, `sending`, `delivered`, `failed`), `attempts`, `responseStatus`, `lastError`, `nextAttemptAt`, `deliveredAt`.

Delivery: the event is enqueued inside the settlement's own transaction, so a settled turn and its notification can never diverge, and a replayed settlement enqueues nothing twice. A background worker claims due rows (`FOR UPDATE SKIP LOCKED`, so workers never double-deliver), signs the stored payload bytes with the endpoint's secret and POSTs it with `x-oxsum-event`, `x-oxsum-delivery` and `x-oxsum-signature` headers. The signature is Stripe-shaped: `t=<unix>,v1=<hex>` where `v1` is HMAC-SHA256 of `"{t}.{body}"` — a receiver recomputes it over the raw body and compares in constant time; reject anything older than a few minutes. A non-2xx answer or an unreachable receiver reschedules on a backoff (10s, 1m, 5m, 15m, 30m, hourly) and the row is `failed` after ten attempts; a worker that dies mid-flight loses its lease after two minutes and the delivery is re-claimed.

The `request.settled` envelope is `{id, type, created_at, org_id, data}`; `data` names the request (`requestId`, `model`, `channel`), the settlement (`kind`, `chargedMinor`, `freezeMinor`, `settlementEntryId`) and the usage (`inputTokens`, `outputTokens`) — enough to recompute or dispute the bill without another call. Every other event shares that envelope shape. `deposit.credited` carries `{rail, paymentRef, amountMinor, entryId}`; `deposit.reversed` carries `{paymentRef, amountMinor, reversalEntryId}` and fires when the operator reverses a credited deposit (issue #356); `deposit.disputed` carries `{rail, depositId, dispute}` and fires when a rail's `dispute_opened` webhook lands (issue #360) — accepting the dispute is the ordinary reversal path. `deposit.failed` stays in the catalog for the rails that report one. `balance.low` carries `{availableMinor, thresholdMinor}`. `statement.finalized` carries `{statementId, period, totalMinor, creditDrawnMinor, dueDate}`; `statement.payment_overdue` the same minus `totalMinor`; `statement.payment_status_updated` carries `{statementId, period, paymentStatus}`. `org.credit_limit_exceeded` carries `{organizationId, creditLimitMinor}` and fires only for an organization with a configured credit line — a self-funded wallet's 402 is ordinary insufficient funds. `anomaly.detected` fires on the settlement kinds the anomalies page reviews (`unpriced`, `capped`, `estimated`, `client_cancelled`, `swept`, `released`) plus a hold dead-lettering; a clean settle enqueues nothing. It also carries upstream price drift (issue #222): when the scheduled `sync-upstream-prices` job's probe finds a stored `upstream` rate disagreeing with the channel's live catalog, the event fans out platform-wide — one delivery per enabled endpoint *subscribed to* `anomaly.detected`, across every organization — with `data` `{kind: "upstream_price_drift", channel, model, field, storedMinor, liveMinor}`, the rate's wire field name and both sides in minor units. `budget.exhaustion_forecast` carries `{keyId, spendLimitMinor, budgetDuration, windowStart, committedMinor, projectedExhaustionDate, daysRemaining}`: a settle attributed to a periodically-limited key recomputes the window's burn rate, and when the projection crosses the limit inside the window it enqueues the event — once per key per window, latched on the key row, re-armed by a constraint `PATCH` or the next window. `budget.threshold_crossed` carries `{scope, keyId|memberId|organizationId, limitMinor, committedMinor, consumedPercent, thresholdPercent, windowStart}`: each settle re-evaluates the key's `spendLimitMinor` (windowed by `budgetDuration`, cumulative without), the minting member's `budgetLimitMinor`, and the organization's credit line — one delivery per armed line per window, latched by the `budget_threshold_fires` row inside the settle's transaction (issue #232). `task.succeeded`/`task.failed` carry `{taskId, kind, model, chargedMinor}` and fire inside the task's terminal settle — `cancelled` ends quietly, since the caller asked (issue #503). All are enqueued inside the state change's own transaction except `org.credit_limit_exceeded` — a refused hold has none, so it announces at-least-once from the dispatch layer.

## Programmatic reads

The programmatic read surface exposes scoped customer data without scraping HTML (issue #146). Balance and budget require a personal session; usage, receipts and request records also accept consumption keys within their own attribution scope.

- `GET /api/v1/budget` — the budget-consumption snapshot (issue #232): every scope a caller can read with `committedMinor` (settled spend plus outstanding holds) and `consumedPercent` floored against its limit. `organization` carries the credit line beside `availableMinor`/`reservedMinor`; `member` is the acting member's own `budgetLimitMinor` — null for a seat with no limit; `keys` lists each live key in the principal's scope with its `spendLimitMinor`, `budgetDuration`, `windowStart` (the current window's UTC start; null for a cumulative limit) and committed figure.
- `GET /api/v1/usage?from=…&to=…` — the `usage_daily` rollup as rows: one per `(day, keyId, channel, model)` with `turns`, the four token sums and `chargedMinor`. `from`/`to` are inclusive `YYYY-MM-DD` dates; `to` defaults to today and `from` to 31 days earlier, and the window is bounded to 92 days.
- `GET /api/v1/billing-records?before=…&limit=…` — settled turns newest-first, one page at a time: `bookedOn` as an ISO `YYYY-MM-DD` UTC calendar date, `requestId`, `entryId`, `model`, `status`, `failureReason` (complete inline text, absent on a clean settle or a long v15 failure), `failureReasonCommitment` (exact SHA-256 and UTF-8 length when the complete reason is retained separately), `kind`, `inputTokens`, `outputTokens`, `chargedMinor`, `keyId`, `discountPercent` (absent when none applied), `upstreamAttempts` (the upstream calls the turn's one hold made — above 1 means it failed over between routes or waited out a bounded `Retry-After`), and `nextCursor` while more remain. `limit` is 1–200, default 50; `before` resumes above a log index. `status` is the turn's terminal state — `settled` for every settled outcome including the anomalous kinds, `errored` for `upstream_error`/`upstream_unreachable`/`swept`, `refused` for a request admission turned away before a hold (issue #233): a refusal writes a self-balancing marker entry of kind `refused` carrying the caller-facing reason, so a 4xx or 429 appears on the billing page and verifies like any other record. Each row's `entryId` is its settlement entry's own id — `GET /api/v1/entries/{entryId}/proof` answers the proof of that charge straight off the row (issue #236).
- `GET /api/v1/requests?keyId=&model=&channel=&from=&to=&status=&minCharge=&limit=&cursor=` — the request log (issue #505): one row per gateway request, settled *or* refused, newest-first and keyset-paged. Each row is `requestId`, `entryId` (the settlement entry the charge proves against — `GET /api/v1/entries/{entryId}/proof` answers its bundle), `keyId`, `modelRequested`/`modelServed` (equal until a cross-model fallback rewrote the served name), `channel`, `admissionOutcome` (`admitted` when the turn reached a hold, `refused` when admission turned it away), `status` (`settled`/`errored`/`refused`), `ttftMs`/`totalMs` (absent on a refusal that never reached upstream), `inputTokens`/`outputTokens`/`cachedTokens`, `chargeMinor` (0 on a refusal), `upstreamAttempts`, `errorKind` (the settlement kind or refusal reason on a non-settled turn), `httpStatus`, `createdAt`, and `nextCursor` while more remain. `httpStatus` is the selected gateway response status, not the provider status: failures before a response carry the returned error status, while an SSE response already started stays 200 even when its body later fails. If the handler is cancelled before selecting a response status, the field is absent. The read is scoped to the caller's organization — `organizationId` is never a member filter and never appears on the wire. `status` accepts `settled`, `errored` or `refused`; `from`/`to` are RFC 3339 instants; `minCharge` keeps rows billed at or above it; `cursor` echoes a page's `nextCursor` — `<unix nanos>:<request_id>`, opaque to the caller. A refused turn still logs a row (`admissionOutcome: refused`, `chargeMinor: 0`, `errorKind` naming the cause), which is what makes the surface answer "which requests were turned away" without a ledger join. `limit` is 1–200, default 50; a malformed `status`, `from`/`to` or `cursor` is `VALIDATION_ERROR`.
- `GET /api/v1/operations?state=&limit=&cursor=` — the durable operations list (issue #601): one row per billable turn the organization admitted, newest-first and keyset-paged on `<unix nanos>:<operation_id>`. Each row is `operationId` (the gateway turn's request id, or the task's `taskId`), `keyId`, `surface` (`chat`, `responses`, `messages`, `embeddings`, `rerank`, `task:{kind}` or `metering` — a privileged service-credential event, which carries no `keyId`), `boundMinor` (the frozen upper bound), `holdKey` (the `open_holds` watch row the operation binds while open), `state` (`created` → `leased` → `running`, then terminal `succeeded`/`failed`/`cancelled`/`timed_out`), `chargedMinor`/`outcomeKind` (present once terminal — a cache hit records `cache_hit` rather than `served`, a metered event `metered`), `model`, `channel` (once one answered), `createdAt`/`completedAt`, and `nextCursor` while more remain. A session reads every operation in its organization; a consumption key reads only the operations its own credential admitted — the same scope `GET /api/v1/requests` keeps — and `organizationId` is never a member filter. `state` accepts only the stored spellings; `limit` is 1–200, default 50; a malformed `state` or `cursor` is `VALIDATION_ERROR`.
- `GET /api/v1/operations/{operationId}` — one operation with its `attempts`: every `request_attempts` row the turn made, each `attemptId`, `channel`, `credentialId`, `externalId` (the upstream's own response id, when the answer carried one), `status` (`started`/`answered`/`refused`/`unreachable` — `answered` is the one the settlement bills), `usageJson` (the upstream's usage report, on the winning attempt), `error`, `startedAt`/`completedAt`. The scope is the list's: a foreign id answers 404 — an operation's existence is not an oracle for another key.
- `POST /api/v1/estimate-price` — `{"model", "mode"?, "inputTokens"?, "outputTokens"?, "serviceTier"?}` answers `{estimateMinor, model, version}`: the number the gateway's hold path would freeze for that shape, computed by the same dearest-applicable-set arithmetic over the dearest route serving the model under the named billing `mode` (absent means `chat`; `outputTokens` is unused by the input-only modes). Nothing is held or charged, an unknown model — or a model with no route priced in the mode — is `VALIDATION_ERROR`, and a real settle never exceeds the estimate.
- `GET /api/v1/pricing` — the public catalog: every model's current price version *per route* flattened into a row with `channel`, `protocol`, `version`, `weight`, `createdAt` and the price fields (`inputPricePerMillion`, `outputPricePerMillion`, `maxOutputTokens`, the cache and reasoning rates, `costPerRequest`, `mode`, `rules`). A model several channels serve lists one row per channel, heaviest weight first. The query never selects channel credentials — the catalog carries nothing to withhold.

Consumption keys read only their own attributed records. Member sessions read their own keys (including revoked/expired keys) plus shared unattributed history; account-wide document markers remain owner/admin-only. Owner/admin sessions read all customer records. Lists filter before pagination, raw proofs return 404 outside scope, and exports/WebSocket use the same policy. See [the route inventory](customer-capabilities.md).

Signed merchant callbacks (#759) preserve their original normalized payload
and digest. When local payment/customer metadata is absent, intake may derive
only those missing coordinates from an already-bound provider payment inside
the route-selected merchant and version. Conflicting supplied IDs remain
rejected; malformed UUID metadata is a validation error. A missing reference
stays pending for recovery. This applies to both versioned and environment
intake, including old Hyperswitch refund payloads that do not copy payment
metadata. It cannot select another merchant's secret or authorize a new
payment. New refund requests send separate refund/order/customer identifiers.

## Protocol-native gateway (`/v1` and `/v1beta`)

Point an OpenAI client's `base_url` at `/v1` and everything else stays the client's own. A Gemini client gets the same at `/v1beta`, the path Google's SDKs build.

- `GET /v1/models` lists the models this deployment serves — exactly those with a relayable price (an `event`-mode price is a metering billable code, not a servable model), once each even when several channels serve a model. `owned_by` is the route with the greatest weight — the one most likely to lead a request — and `created` is when the version in force was written. Model aliases list beside the direct names (issue #286): `owned_by` is the channel that claims the alias and `created` is when it was claimed.
- A `model` name may be an alias — a public name an operator claimed on one channel to rename the upstream model it relays (issue #286). The alias resolves to the claiming channel alone (an alias is a rename, not a routing layer), the request is forwarded upstream under the `upstreamModel`, and the turn is priced by the alias's own price row when the channel wrote one — the upstream model's row otherwise. The bill names the alias in `model` and the served model in `upstreamModel`.
- `POST /v1/chat/completions` relays one turn, streaming or not, and bills it — served by channels whose protocol is `openai`. Fields the gateway does not act on are forwarded upstream unchanged, so a client that works against the provider works here. `max_tokens` is set to the output upper bound the freeze was computed for, `max_completion_tokens` is dropped, and `stream_options.include_usage` is forced on for a stream. Three fields are recorded as the caller's attribution on the turn's usage record and forwarded unchanged — `user` (a string of at most 128 characters), `metadata` (at most ten string-valued pairs, keys and values at most 64 characters) and `service_tier` (a string of at most 64); anything past a bound is a 400 naming the field, because silently truncating an id would bill it under the wrong name.
- `POST /v1/messages` is the same turn in Anthropic's Messages shape — served by `anthropic` channels and answered in Anthropic's envelope, errors included (`{"type": "error", "error": {…}}`, with oxsum's code kept on `error.code`). `system` accepts a string or text blocks; a `text` content block contributes its `text`, a `tool_use`/`tool_result`/`thinking` block its serialized JSON, and a media block (`image`, `document`, `audio`, `video`) is refused 400 — the input bound must stay computable. `metadata.user_id` is the attribution field here. Upstream is called at `{baseUrl}/messages` with the channel credential as `x-api-key`; `anthropic-version` and `anthropic-beta` pass through, and `anthropic-version` defaults to `2023-06-01`. Usage normalization follows Anthropic's report: `input_tokens` plus `cache_read_input_tokens` plus `cache_creation_input_tokens` is the billed input, and the 5-minute/1-hour write tiers come from `cache_creation.ephemeral_*_input_tokens` (an unsplit lump counts as 5-minute). On a stream `message_start` carries the input side and `message_delta` the cumulative output, ending with `message_stop`. The cost headers, the `Idempotency-Key` semantics, and every refusal status are exactly `/v1/chat/completions`'s.
- `POST /v1/responses` is OpenAI's successor wire (issue #495), served by the same `openai` channels — upstream is called at `{baseUrl}/responses` with the Bearer credential, and errors answer in OpenAI's envelope. `input` is a string or the items list: a `message` item's `input_text`/`output_text` parts contribute their text, a `function_call`/`function_call_output`/`reasoning` item contributes its serialized JSON, and a media part (`input_image`, `input_file`, `input_audio`) is refused 400. `instructions` counts toward the input bound. `max_output_tokens` is the ceiling — the forwarded body always carries the freeze's bound under that name, and a stream forces `stream_options.include_usage` on. `store` and `previous_response_id` are pass-through: the response's state lives in the upstream's store, not in oxsum. Usage settles against `usage.input_tokens`/`usage.output_tokens` (with `input_tokens_details.cached_tokens` and `output_tokens_details.reasoning_tokens` the subsets), and a stream ends on `response.completed`, `response.failed` or `response.incomplete` — the events that carry the terminal usage; there is no `[DONE]` on this wire. The cost headers and `Idempotency-Key` semantics are `/v1/chat/completions`'s.
- `POST /v1/embeddings` relays an embeddings call — OpenAI's shape, served by `openai` channels whose price for the model carries `mode: embeddings`; anything else is `model_not_served`. `input` may be a string, an array of strings, an array of token ids, or an array of token-id arrays: token arrays freeze at their exact count, strings at the text bound. The surface is input-only — `stream: true` is a 400, no `max_tokens` is forwarded — and the turn settles against `usage.prompt_tokens` (`total_tokens` stands in when the prompt count is absent), estimated from the request's own input when no report arrives. Upstream is called at `{baseUrl}/embeddings` with the channel credential as `Authorization: Bearer`. The cost headers, failover, bounded `Retry-After`, `Idempotency-Key` semantics and refusal statuses are the shared pipeline's.
- `POST /v1/rerank` relays a rerank call in the shape Jina and Cohere speak — `query` plus a `documents` array of strings or `{"text": …}` objects — served by `openai` channels pricing the model `mode: rerank`. Query and document texts together give the input bound; usage settles against `usage.total_tokens`, estimated from the request's own input when no report arrives. Upstream is called at `{baseUrl}/rerank`; the pipeline contract is `/v1/embeddings`'s.
- `POST /v1beta/models/{model}:generateContent` and `POST /v1beta/models/{model}:streamGenerateContent` are the Gemini pair (issue #494) — Gemini's wire puts the model in the path and the stream flag in the action (`:streamGenerateContent`, `?alt=sse` supported either way), so the request body carries neither and `stream: true` inside it is a 400; a `model` field that disagrees with the path's is refused, a matching one tolerated. `contents[].parts[]` gives the input bound: a `text` part its text, a structured part (`functionCall`, `functionResponse`, `executableCode`, …) its serialized JSON, and `inlineData`/`fileData` is refused 400 like every media part on `/v1`. `generationConfig.maxOutputTokens` is the output ceiling — absent, the price's `maxOutputTokens` stands in and the bound is written back into `generationConfig` for upstream. `labels` is the attribution field: a map of at most ten string-valued pairs, bounded like `metadata` on `/v1/chat/completions`. Upstream is called at `{baseUrl}/models/{upstreamModel}:{action}` with the channel credential as `x-goog-api-key` (the one spelling AI Studio and Vertex both take), and SSE answers carry no `[DONE]` sentinel — the stream ends on a candidate's `finishReason`. Usage normalization follows `usageMetadata`: `promptTokenCount` already includes `cachedContentTokenCount`, `toolUsePromptTokenCount` is added into input, `candidatesTokenCount` plus `thoughtsTokenCount` is billed output, and the modality breakdown (`promptTokensDetails`/`candidatesTokensDetails`) lands on `usage_details`. The cost headers, idempotency and refusal statuses are `/v1/chat/completions`'s; the error envelope keeps OpenAI's shape.
- `POST /v1/tasks`, `GET /v1/tasks/{taskId}`, `POST /v1/tasks/{taskId}/cancel` are the async task relay (issue #503) — a turn on a minutes timescale, served by `tasks`-protocol channels. Submit freezes the declared `maxCostMinor` (or the price's `costPerRequest` when none is named), relays `{baseUrl}/tasks` with `Authorization: Bearer`, records `external_task_id` and answers `{taskId, submitted}`; the poll is a row read — a `poll-tasks` periodic pass asks `{baseUrl}/tasks/{externalTaskId}` every 30s for the still-open rows — and the terminal answer settles the hold: `succeeded` at the result's `usage` report itemized against the channel price (a reportless success prices a zero-count record, so `costPerRequest` still charges its flat rate), `failed`/`cancelled` at zero with kind `released`. A terminal settle enqueues `task.succeeded`/`task.failed` to the organization's subscribed webhook endpoints; a caller's cancel asks `{baseUrl}/tasks/{id}/cancel` best-effort and releases the hold either way. Only the exact submitting key may poll or cancel; unknown, sibling-key and foreign-customer IDs all return 404 without side effects. The `Idempotency-Key` claim/replay contract is `/v1/chat/completions`'s.
- Errors are OpenAI's `{"error": {"message", "type", "param", "code"}}`, with oxsum's own code in `error.code`. Upstream's error object is passed through verbatim on a 502.
- `Idempotency-Key` makes a retry the same turn: the first request claims the key, and a retry with the identical request body is answered from the record — a finished non-streamed turn replays its stored response, a finished streamed turn answers its settled receipt (`{"object": "oxsum.receipt", ...}` naming the request id, charge and usage), and either replay carries `Idempotent-Replayed: true` and the original `x-oxsum-request-id`. A retry while the first turn still runs is `409 IDEMPOTENCY_IN_FLIGHT`; the same key under a different body, consumption key, protocol or path-model is `422 IDEMPOTENCY_MISMATCH`. Legacy unbound fingerprints also conflict until their original 24-hour expiry. A refusal that never reached the wallet releases the key, and every claim expires 24 hours after it was made. A finished turn whose stored body could not be parsed or exceeded 64 KiB completes the record with a `NOT_REPLAYABLE` marker instead of the body — the replay then answers the original status with that marker as its body: the work happened and was billed once, only its bytes were dropped.
- `x-oxsum-request-id` names a turn, and every turn endpoint sets it: `POST /v1/chat/completions`, `POST /v1/messages`, `POST /v1/embeddings` and `POST /v1/rerank` each stamp it on every response they produce — a settled 200, a 400 for a body that is not JSON, any error they raise — because the id is minted before the body is read. `GET /v1/models` does not set it, and a 401 cannot: the auth middleware answers before any handler runs, so no id exists yet. The entries a turn wrote are `req-<id>:hold` and the settlement derived from it (`oxsum_core::settlement_key_for`), and `oxsum_core::entry_id_for` derives their ids, so the id in a header is enough to name the bill.
- Cost headers answer what a request spent without a second call. Every answer that got as far as the hold — a completion or a post-hold upstream refusal — carries `x-oxsum-freeze-minor` (the reserved bound, the most the request can cost). The customer balance header is retired; neither headers nor insufficient-funds errors disclose it. The settled charge is `x-oxsum-charged-minor`: a header on a non-streamed answer and on a post-hold refusal, and an HTTP trailer on a streamed answer — the head is already out when a stream settles, and the trailer is the channel that leaves upstream's frames untouched. A client that does not read trailers still has the request id and the settled receipt. `x-oxsum-cache: hit` marks an answer the response cache served (issue #497; the settings key keeps its historical `semanticCache` name): the stored body re-served and billed at the cache-read rate, the settlement's `cache` block carrying `similarity: 1.0` — an exact match of the complete admitted request context (issue #645), not a similarity search — and naming the original turn's entry. The header is absent on a miss, a stream, or a channel that never opted in.
- Text content only: an image or another content part is 400, because the freeze needs an input bound that cannot be undercounted.
- How each turn ends is recorded in the settlement entry's description as its `kind`; docs/user-guide.md has the table. The same description is a versioned billing credential — `"v": 11` today — carrying the `channel`, the `priceVersion`, the metered `usage` dimensions, the `matchedRule` when a conditional price hit, the `discountPercent` when one applied, `upstreamAttempts`, the `upstreamModel` when the turn was asked for under an alias (issue #286), the `subscription` block when a plan's quota covered the charge (issue #496), the `cache` block when the response cache served the answer (`{sourceEntryId, similarity}` with `similarity: 1.0` — an exact complete-context match, issue #497/#645), the `guardrail` array when a configured policy acted on the turn (`{policy, verdict, reason, streamed?}` — issue #502), and the `lines` the charge is the capped sum of, each spelled `[item, units, pricePerMillion]` so the credential fits the ledger's description limit — so the charge can be recomputed from inside the entry's own content hash (`oxsum_verify::verify_charge`; docs/decisions.md, T1-2).
- The guardrail pipeline (issue #502) judges a turn in two places. `pre_call` runs after the route is chosen and before the hold is taken: a policy's `block` answers 403 `FORBIDDEN` and the turn costs nothing — only the `refused` marker records it like every other admission refusal — while a `flag` is collected onto the settlement. `post_call` runs on the completed answer: a `block` settles `Charge::Nothing` — the whole hold refunded, because the upstream answer already arrived and the exposure is the org's own — and still answers 403, while a streamed turn's verdicts can only be recorded, which the settlement's `guardrail` entries mark `streamed: true`. A moderation endpoint that cannot answer fails *closed* before the hold (a screening service down must not admit an unscreened prompt) and *open* after it (the upstream cost is real either way).
- A request is priced by the version in force when it starts. A price change during a turn reaches later requests only; the turn in flight settles at the version it began on.
- A model several channels serve fails over inside the one hold: the first attempt is a weighted pick across the routes (`weight` on each route's price version; inside an equal weight tier the cheapest `inputPricePerMillion` leads, issue #438), and a refusal that arrives before any answer bytes — upstream unreachable, 429, or 5xx — rotates to the next attempt. A channel's credential pool (issue #444) tries its own healthy keys first — a 429 or 5xx cools the credential that drew it for a bounded minute and the turn retries on the pool's next key, so one account's rate limit rotates inside the same route before the turn leaves for a differently-priced channel — and a last route's 429 naming a `Retry-After` of at most 3 seconds is waited out once. A 4xx fails fast, and a failure once bytes are flowing is never retried. The settlement names the channel that actually answered and counts the calls in `upstreamAttempts`; which credential inside the channel answered is never recorded — every key of a pool bills identically.

## Metering (`/api/v1/metering`)

Business integrations report external billable events under an `oxs-int-…` bearer with `meteringWrite` and either explicitly selected customers or accounts provisioned in its owner-selected application namespace. A wrong scope is 403; a customer or hold outside the credential is 404. Unknown, expired, revoked, customer, legacy `oxs-svc-…` and wrong-surface credentials are 401. The core rechecks current grants for every operation, including a captured authenticated principal and committed retries. A committed authorization may finish after revocation; subsequent API calls are refused. Recovery remains available to trusted jobs.

The current business owner in business context issues credentials through `POST /api/v1/business/integration-credentials`: `idempotencyKey`, optional `name`, distinct `scopes` (`customersRead`, `meteringWrite`, `customersProvision`, `billingRead`, and application-only `customersLifecycleWrite`), a customer grant, future `expiresAt` within 365 days, and `reason`. Legacy grants select 1–1000 distinct `customerIds`, except a `customersProvision`-only grant may start empty. An application grant selects `applicationId` and empty `customerIds`; it reaches only accounts provisioned inside that application. No wildcard or unsupported scopes. Secrets are random 32-byte values, SHA-256 at rest, returned once and null on exact replay. Changed terms conflict. Current session/TOTP/owner authority, immutable receipt, grants and audit share a transaction. `GET` lists metadata with `after` UUID/`limit` 1–100; `POST /{credentialId}/revoke` takes a new `idempotencyKey` and `reason`. Rotation issues a new credential before revoking the old one. Application credentials retain their namespace and can finish its authorized metering holds with current scope; explicit legacy credentials remain separate.

`customersRead` authorizes `GET /api/v1/business/customers` and `/customers/{organizationId}`, filtered before pagination, and application external-ID lookup through `/api/v1/integration/customers`. It grants no mutations, billing exports, staff/provider secrets, personal session or gateway access. The separate `billingRead` scope serves selected customer balance, log and bill routes under `/api/v1/business/customers/{organizationId}`. Owner issuance is required for every scope; no funds-crediting scope is implemented.

- `POST /api/v1/metering/holds` — `{"idempotencyKey", "organizationId", "channel", "billableCode", "eventId"?, "usage"}` freezes the declared usage's fully itemized cost: `billableCode` resolves the latest `mode: event` price the named `channel` carries (the code is the price book's model name on that channel), and the answer is `{holdKey, freezeMinor, priceVersion}` — the version pinned to the hold so a later price change cannot move it. `usage.eventType` is required and names what the event is; a dimension the price cannot bill is refused, not partial-billed. The hold rides the sweeper's watch table — one that never settles releases at zero as `swept` — and suspension (`403`) and insolvency (`402`) refuse like every hold. Replaying the key answers the original hold, pinned version included.
- `POST /api/v1/metering/settle` — `{"organizationId", "holdKey", "eventId"?, "usage"}` charges the actual usage at the hold's pinned price version, capped by the freeze: `{chargedMinor, kind, priceVersion, settlementId}` where `kind` is `usage` or `capped`. The settlement's entry id derives from the hold key (`entry_id_for(settlement_key_for(holdKey))`), so a hold settles at most once and a retry replays the committed answer — even after the watch row is gone. A hold not outstanding — of this organization — is `404`; settling a released one is `409`.
- `POST /api/v1/metering/release` — `{"organizationId", "holdKey"}` lets a hold go at zero and answers the same shape with `kind: released` — the event that never ran. Replaying it answers the committed release; releasing a settled hold is `409`.
- `POST /api/v1/metering/settlements` — the one-shot shape: `{"idempotencyKey", "organizationId", "channel", "billableCode", "eventId"?, "usage"}` freezes and settles an already-completed event in one call, under an opaque hold key derived from application (or legacy credential), customer and idempotency key. The hold is watched for the operation's span so a crash between the two writes is still swept, and a retry replays the committed outcome.

An authorization row pins the original operation, body, price/discount and freeze before money moves. Keys are scoped to `(credentialId, organizationId, idempotencyKey)`; changing operation/body conflicts. Settle/release binds the finalization body once; changed usage/eventId or another operation conflicts even after settlement. Exact replay preserves the original result after price changes. Caller `eventId` remains a settlement label, while the usage projection and sweeper use the scoped hold key, so labels cannot collide across customers. `endUser` is attribution only.

Every metered settlement writes a usage row with `upstreamAttempts: 0` — nothing was relayed — and a settlement description at schema v6: the reporting service's name in `service`, the `eventType`, the pinned `priceVersion` and the itemized `lines`, so the organization's bills page and `verify_charge` recompute it like every other charge. A service's entries attribute to `svc-<credentialId>` rather than to any key or user.

## Business provider configuration (`/api/v1/business/channels`)

Current business-context personal sessions manage provider configuration. The owner can set a channel's connection/protocol, replace/add/disable/remove provider credentials, and set or clear its billing credential. Owner/admin can append versioned prices, claim/remove aliases, configure the existing cache policy and set model fallback chains. Other roles, consumption keys, integration credentials and deployment bearers cannot use these business routes. Deployment-token routes remain the separately authenticated recovery surface.

Every mutation takes `{idempotencyKey, reason, config}` and returns `{id, resourceId, version}`. Nullable result fields remain present. The complete typed request is bound to a keyed digest; plaintext secrets are neither audited nor logged. Exact retry returns the original receipt, including its original price version or credential ID, even after later changes; altered input returns 409. Current session/TOTP/role is checked before replay. Configuration, result and immutable audit share one transaction. Use a new key only for a deliberate new operation. A deployment sealing-key rotation makes pre-rotation request fingerprints conflict safely.

`GET /channels?after=<uuid>&limit=50` filters business ownership before keyset pagination (limit 1–100). Prices and aliases use `GET /channels/{channelName}/prices|aliases`; the fallback read is `GET /model-fallbacks/{model}`. Metadata has no plaintext/sealed key; legacy error text, URL credentials/query/fragment and unknown cache settings are excluded. Provider metadata is owner/admin only.

`POST /channels` takes `config: {name, baseUrl, protocol, apiKeys?, destinationPolicy?}`. A new channel needs 1–100 keys. Omitted/null `apiKeys` preserves an existing pool; an empty pool fails. Destination/protocol changes require explicitly supplied keys and clear the existing billing key. New endpoints reject embedded URL credentials, query and fragment. Saves make no provider call. Credentials require 8–8192 bytes with no controls; only their last four characters are displayed.

Channel mutations append to `/channels/{channelName}`: `POST /credentials` (`{apiKey}`), `/billing-credential` (`{apiKey}` or `{apiKey: null}`), `/credentials/{credentialId}/status` (`{status: active|disabled}`), `/credentials/{credentialId}/remove` (`{}`), `/cache` (`{policy}` or `{policy: null}`), `/prices` (`{model, price, weight}`), `/aliases` (`{alias, upstreamModel}`), and `/aliases/{alias}/remove` (`{}`). The `price` object uses existing integer price fields and requires explicit input/output/max-output fields. Alias targets must be served by that channel. The last active pool member cannot be removed; disable it explicitly when intentionally stopping service. Foreign channel/credential lookups return 404.

`PUT /model-fallbacks/{model}` takes `config: {fallbacks: [...]}`: at most 16 distinct owned served model/alias names, excluding the requested model. Empty clears. Cache policy accepts required `enabled` and optional `ttlSeconds`; the legacy `excludeSystem` field is accepted but ignored (the complete request always participates), and invalid/unknown fields fail before mutation. Channel identity, historical price rows and already resolved Serving snapshots remain intact after changes.

`destinationPolicy` is optional connection configuration: `{network: "public"}` (default) requires HTTPS and only public DNS/IP addresses; `{network: "internal", allowedIps: ["10.0.0.10"]}` explicitly approves exact IPv4/IPv6 addresses for this endpoint. Internal allows HTTP(S); it never permits link-local/metadata, unspecified, multicast, mapped/transition or reserved destinations. All DNS answers must satisfy policy. The connector pins that answer set, preserves the original hostname for TLS, disables proxies and redirects, and never forwards credentials to a redirect target. A new endpoint/protocol resets policy to public unless the owner supplies new approval. Deployment recovery accepts the same field. Existing channels without policy receive the public default; an internal legacy endpoint needs explicit reauthorization. No DNS lookup or network connection runs on save.

`POST /channels/{channelName}/connection-check` takes `{idempotencyKey, reason, config: {}}` and requires current owner authority. It commits audit intent before sending one HEAD to the configured base URL under its policy and credential. It sends no inference prompt. Result `{id, channelId, state, httpStatus}` distinguishes pending, reachable, authenticationFailed, httpError, blocked and unreachable. A 405 means the endpoint was reached but HEAD is unsupported; reachable does not certify model or credential functionality. Redirects are httpError and never followed. Exact retries return the same check without another request; an interrupted pending intent remains pending and a deliberate new check requires a new key. Only status categories/codes are stored or returned, never provider response bodies or transport URL details.

New asynchronous tasks pin endpoint, policy and sealed submitting credential; poll/cancel keep that snapshot after a channel is repointed. Migration 0079 captures the upgrade-time directory for old tasks because earlier submission coordinates were not stored; it does not invent that history or grant internal-network authority. Customer sale books are managed separately through the sale-book API below. Model/provider certification remains a separate release gate.

## Platform admin (`/api/v1/admin`)

Whoever deploys oxsum, and nobody else: the credential is the operator token from `OXSUM_ADMIN_TOKEN`, sent as a bearer token, compared in constant time, and refused unless it is at least 16 characters when the server starts. An organization's API key does not open this surface, and this token does not open an organization's.

- `GET /api/v1/admin/channels` — the channels, each with the current version of every model it serves and its credential pool (issue #444): `credentials` lists one row per upstream key — `credentialId`, `apiKeyLast4`, `position`, `status`, `cooldownUntil`, `lastError`, `lastUsedAt`, `createdAt` — and only the last four characters of a credential are ever returned.
- `POST /api/v1/admin/channels` — create a channel (name, baseUrl, `apiKey` or `apiKeys`, optional `protocol`), or replace the connection of the channel that already has that name. `apiKey` is the single-key spelling, merged into `apiKeys` when both arrive; either carries the channel's whole credential pool — a `set` re-credentials the channel wholesale, so every key the pool should hold goes in one call — and an absent pair leaves the stored pool untouched at the same endpoint/protocol while a present empty `apiKeys` is `VALIDATION_ERROR` (a channel with no key cannot serve). `protocol` names the upstream wire protocol the channel speaks — `openai` or `anthropic` — which selects the adapter that normalizes its usage reports and the `/v1` surface its models are served on; it defaults to `openai`, and any other name is `VALIDATION_ERROR`, because a channel that cannot normalize its own usage reports cannot serve. `billKey` and `semanticCache` (issue #497) are the triple-state companions: absent leaves the stored value alone, explicit `null` clears it, a value replaces it — `semanticCache` is the `{enabled, ttlSeconds?, excludeSystem?}` policy under `settings.semanticCache` — `excludeSystem` is accepted for compatibility but has no effect, because the complete request always participates in the cache key — and a non-object or malformed policy is `VALIDATION_ERROR`, the same refusal the gateway would raise at read time. Prices are untouched by a connection change.
- `POST /api/v1/admin/channels/{channelName}/credentials` — append one upstream key to a channel's credential pool (issue #444): `{"apiKey"}`, answered as the stored `ChannelCredential` row; the key lands sealed at the pool's last position, `active` and healthy. `NOT_FOUND` for a channel that does not exist.
- `PATCH /api/v1/admin/channels/{channelName}/credentials/{credentialId}` — switch one credential's `status`: `{"status"}` is `active` (picked again) or `disabled` (kept on record, never picked — the operator's way of taking a key out of the pick without losing its health state). `NOT_FOUND` for a credential that is not this channel's; `VALIDATION_ERROR` for any other status.
- `DELETE /api/v1/admin/channels/{channelName}/credentials/{credentialId}` — drop one credential from the pool; the delete is hard, like every other routing-configuration delete. `NOT_FOUND` for a credential that is not this channel's. The pool's last `active` credential refuses `VALIDATION_ERROR` — a channel with no pickable key cannot serve, so it stays until the operator disables it or retires the channel (a cooling credential still counts as pickable: its window ends on its own).
- `POST /api/v1/admin/channels/{channelName}/prices` — append a price version for a model, and answer the version that was written. The body is the whole price set: `inputPricePerMillion` is required; `outputPricePerMillion` and `maxOutputTokens` are required on a `chat` price and must be absent or zero on the input-only modes — an absent output side is never silently free. `cacheReadPricePerMillion`, `cacheWrite5mPricePerMillion`, `cacheWrite1hPricePerMillion`, `reasoningPricePerMillion`, `costPerRequest` and an `upstream` cost set are optional dimensions, `mode` (`chat`, `embeddings`, `rerank`; absent means `chat`) names the billing mode and the `/v1` surface that may serve the model, and `rules` lists conditional sets — a `match` (`serviceTier`, `minInputTokens`, `maxInputTokens`) swaps in the rule's whole set, most-specific-wins. `weight` (1–1000, default 100) is the route's relative preference for leading a request, versioned with the price it rides on — a weight change alone appends a version. A model may be priced on several channels: each `(channel, model)` pair is its own append-only history. `VALIDATION_ERROR` for a malformed rule, two rules that could match the same request at the same specificity, a weight outside the bound, or a mode's field rule violated — an input-only price naming an output side, a chat price missing one.
- `GET /api/v1/admin/channels/{channelName}/prices` — every version of every model of that channel, newest first. This is the history, and it is what makes the `priceVersion` in an old settlement checkable.
- `GET /api/v1/admin/channels/{channelName}/aliases` — one channel's aliases, oldest first (issue #286). Each row is `{alias, channel, upstreamModel, createdAt}` — the public name callers write and the upstream model the channel relays it as.
- `POST /api/v1/admin/channels/{channelName}/aliases` — claim an alias for a channel: `{"alias", "upstreamModel"}`, answering the written row. One name answers one route set: an alias already claimed on any channel, or a name another channel serves directly, is `CONFLICT` — and an alias on the same channel twice is too. `NOT_FOUND` for a channel that does not exist, `VALIDATION_ERROR` for an empty or oversized name.
- `DELETE /api/v1/admin/channels/{channelName}/aliases/{alias}` — drop one of the channel's aliases. Aliases are routing configuration, not ledger data, so the delete is hard — bills already written still name the alias. A name that is not this channel's — unknown to it, or claimed by another channel — is `NOT_FOUND`. Both writes land in the audit log as `channel.alias.set`/`channel.alias.delete`.
- `GET /api/v1/admin/guardrail-policies` — every configured guardrail policy (issue #502), ordered by name: `{name, kind, config, createdAt}`. A `moderation` policy's bearer is stored sealed and never serialized — the list carries the rule, never the credential.
- `PUT /api/v1/admin/guardrail-policies/{name}` — write or replace a policy: `{kind, config, apiKey?}`. `name` is the address a binding or a key's `requiredPolicies` references, matching `^[a-z0-9][a-z0-9._-]{0,63}$`. The kind picks the config's shape: `keyword` takes `{patterns: [{re, action}]}` (each `action` `flag` or `block`, compiled at write — an uncompilable regex is `VALIDATION_ERROR`, not a rule every turn would skip); `pii` takes `{}`; `moderation` takes `{endpoint, timeoutMs?}` — an absolute http(s) URL the request's texts are posted to — plus `apiKey`, the endpoint's bearer, sealed under the deployment's `OXSUM_SECRET_KEY` the way an upstream key is. An absent `apiKey` keeps a stored bearer, so editing a moderation policy's endpoint does not demand its key again. Lands in the audit log as `guardrail.policy.set`.
- `DELETE /api/v1/admin/guardrail-policies/{name}` — remove a policy; an unknown name is `NOT_FOUND`. Bindings and keys that still name it keep the dangling name — the gateway skips what does not resolve rather than refuse the turn, so a retired policy unguards quietly until its references are updated. Audit action `guardrail.policy.delete`.
- `GET /api/v1/admin/guardrail-bindings` — every scope's policy set, ordered by `(scope, target)`.
- `PUT /api/v1/admin/guardrail-bindings/{scope}/{target}` — replace one scope's binding: `{policies}` is the ordered list of policy names, `scope` is `channel` (target the channel's name) or `key` (target the key's id), and every name must already exist — `VALIDATION_ERROR` on a dangling one, because a binding that names nothing would silently unguard the turns it claims to protect. Audit action `guardrail.binding.set`.
- `DELETE /api/v1/admin/guardrail-bindings/{scope}/{target}` — remove one binding; unknown is `NOT_FOUND`. A `PUT` with `policies: []` writes the same "runs nothing" — the delete exists so the row does not linger as configuration noise. Audit action `guardrail.binding.delete`.
- `GET /api/v1/admin/model-fallbacks/{model}` — one model's fallback chain (issue #500): `{"fallbacks": [...]}` in try order, empty for a model with none.
- `PUT /api/v1/admin/model-fallbacks/{model}` — replace one model's whole chain atomically: `{"fallbacks"}` is the ordered set of models a request may serve under when `model`'s channels all fail. The chain is model→model, channel-agnostic — each name resolves through the same `servings_named` its own name produces — so every name must resolve to a served model or alias (`VALIDATION_ERROR` on an unserved name, which would dead-end a request that already failed once), and `model` may not appear inside its own chain. `model` itself must be served. An empty `fallbacks` clears the chain. Audit action `model_fallback.set`.
- `DELETE /api/v1/admin/model-fallbacks/{model}` — drop one model's chain; a model that had none is `NOT_FOUND`. The chain is routing configuration, not ledger data, so the delete is hard — a settled bill still names the model that answered. Audit action `model_fallback.delete`.
- `GET /api/v1/org/groups` lists the authenticated organization's groups read-only for members and keys. Each `{id, name, channelIds, channels}` includes readable `{id, name}` channel identities, so key owners can inspect the effective access set without resolving UUIDs. `GET /api/v1/admin/channels` includes each channel's stable `id` for group membership.
- `GET /api/v1/admin/groups?organizationId=` — one organization's request groups (issue #508), oldest first: `{id, name, channelIds, channels}` each — the named channel subset a grouped key may route through plus readable channel identities. `organizationId` is required (`VALIDATION_ERROR` without it).
- `PUT /api/v1/admin/organizations/{organizationId}/groups/{name}` — create or replace a group's whole channel set, idempotent on `(organization, name)`: `{channelIds}` is the complete list, each id checked to be a real channel (`VALIDATION_ERROR` on a channel id that is not — a group naming a channel that can never serve would scope keys into silence). `name` is 1–64 bytes. Audit action `group.set`.
- `DELETE /api/v1/admin/organizations/{organizationId}/groups/{name}` — drop a group; an organization with no group of that name is `NOT_FOUND`. Deletion returns 409 `CONFLICT` while any key remains assigned (including revoked keys); reassign or explicitly ungroup them first. Audit action `group.delete`.
- `GET /api/v1/admin/organizations` — every organization, oldest first, with its kind, headcount and what its wallet shows (`availableMinor`, `reservedMinor`), plus the credit line it carries (`creditLimitMinor`, `creditUsedMinor`) and its suspension (`suspended`, `suspendedAt` — null while the organization runs normally).
- `PATCH /api/v1/admin/organizations/{organizationId}` — update the organization's billing terms and suspension: `{"creditLimitMinor"?, "paymentTermsDays"?, "tier"?, "suspended"?, "dunningGraceDays"?, "displayCurrency"?, "idempotencyKey"}`, at least one of the six. `displayCurrency` is the organization's fiat rendering (issue #257): `{"code", "ratePerCredit", "decimals"}` — absent leaves it, `null` clears it, a value sets it, and a malformed pair is `VALIDATION_ERROR`. The conversion is presentational only; the ledger keeps integer minor units either way. `tier` and `dunningGraceDays` are triple-state fields: absent leaves the assignment alone, explicit `null` clears it, and a value sets it — `tier` names a profile (an unknown name is `VALIDATION_ERROR`), `dunningGraceDays` the days past a finalized statement's `dueDate` before the `statements` job's dunning sweep suspends the organization's spend (issue #258; `null` switches the automation off). The tier gates admission only; it is never a pricing input. The limit is spendable credit a hold may draw after granted and purchased credit run out; `availableMinor` counts undrawn credit, and `GET /api/v1/balance` answers `creditLimitMinor`/`creditUsedMinor` beside the pools' sums. Shrinking the limit below the outstanding draw is `VALIDATION_ERROR` — the repayment is a top-up, which credits the drawn line before the purchased pool. `0` is allowed once the line is repaid. `paymentTermsDays` is how long a finalized statement's payment has before it falls due, snapshotted onto each statement at issue. `suspended` suspends (`true`) or reinstates (`false`) the organization's spend: a suspended organization's new holds refuse `FORBIDDEN` on `/v1` and `/v1/messages`, while holds already open still settle and money-in still lands; suspending enqueues `org.suspended` for the organization's subscribed webhook endpoints, a replayed flag announces nothing, and manual suspension and dunning remain independent: clearing one never clears the other. The answer adds `dunningGraceDays` and `dunningSuspendedAt` — null when the sweep automation is off and when no sweep-set suspension stands. `NOT_FOUND` for an organization that does not exist. Replaying the idempotency key answers the original result; a credit-limit change itself is one ledger entry — the whole history of a limit is inside the verifiable log.
- `POST /api/v1/admin/organizations/{organizationId}/adjustments` — book a signed amount into the organization's wallet: `{"amountMinor", "reason", "idempotencyKey"}`. Positive grants, negative deducts; the reason is required and becomes the entry's description.
- `GET/POST /api/v1/admin/organizations/{organizationId}/promo-lots` — the organization's promotional lots (issue #609). `POST` grants one: `{"amountMinor", "expiresAt"?, "reason"?, "idempotencyKey"}` — the idempotency key is the lot's `sourceRef` under `source: "admin"`, so a retried grant answers the standing lot rather than granting twice; absent `expiresAt` never expires. The lot's row and its `debit Adjustments / credit Promo` entry commit in one transaction. `GET` lists the organization's lots soonest-expiring first (`?includeClosed=true` keeps expired and cancelled rows), each `{lotId, organizationId, source, sourceRef, amountMinor, remainingMinor, reservedMinor, expiresAt, createdAt, closedAt, closeReason}` — `remainingMinor` what new holds can still pin, `reservedMinor` what open holds already named. A lot's money draws before every other pool at admission, expires on the `expire-lots` pass burning the unreserved remainder to `Equity:PromoExpiry`, and can never refund to cash — a release into a closed lot burns there too.
- `POST /api/v1/admin/organizations/{organizationId}/promo-lots/{lotId}/cancel` — cancel a lot's unspent remainder (`{"reason"?, "idempotencyKey"}`): the unreserved part burns to `Equity:PromoExpiry` and the row closes `cancelled`, the part open draws still reserve stays theirs until their holds settle. Idempotent by `close_reason` — a repeated cancel answers the standing closed lot. A lot of another organization is `NOT_FOUND`, the same scoping every org-scoped write applies. The promo side of a refunded bundle unwinds this way; the purchased side unwinds through the deposit reversal above.
- `GET/POST /api/v1/admin/organizations/{organizationId}/credit-notes` — a credit note (issue #352, decision D12): a reasoned credit that posts one ledger entry (Revenue debit / Wallet credit) and moves no money. `POST` takes `{amountMinor, reason, idempotencyKey}` (positive amount, required reason) and answers the note with the balance after it; `GET` lists an organization's notes newest first. A retried key answers the standing note. It is not a refund — no PSP is called and no deposit is reversed. `VALIDATION_ERROR` for a zero amount or an empty reason, `NOT_FOUND` for an organization that does not exist, `INSUFFICIENT_FUNDS` for a deduction the balance cannot carry — the ledger's no-overdraft rule, not a read-then-write check. Replaying the idempotency key answers the same entry rather than writing a second.
- `GET /api/v1/admin/subscription-plans` — every subscription plan the deployment defines (issue #496), in `sortOrder` — the catalog order that also decides whether a rebind closes the old period as `upgrade` or `downgrade`. Disabled plans list too; they cannot be bound or checked out.
- `PUT /api/v1/admin/subscription-plans/{name}` — upsert a plan by its unique catalog name: `{quotaMinor, resetPeriod, allowWalletOverflow?, priceMinor?, stripePriceId?, enabled?, sortOrder?}`. `quotaMinor` (int64 minor units) is the per-period credit quota; absent or `null` unmeters the plan — every request draws in full. `resetPeriod` is `daily`, `weekly`, `monthly` or `never` (no period end at all: nothing rolls, nothing expires). `allowWalletOverflow` (default `true`) decides what an exhausted quota does — `true` holds the overflow against the wallet, `false` refuses the gateway request. `stripePriceId` makes the plan checkout-able; `null` keeps it admin-bound. Editing a plan never re-meters a running binding — `quotaMinor` and `allowWalletOverflow` snapshot onto the binding and each period at bind and rollover time. The write is whole-row and replay-safe, so it carries no idempotency key.
- `PUT /api/v1/admin/organizations/{organizationId}/subscription` — bind a plan to the organization: `{planId, source?, stripeSubscriptionId?}` answers the `OrgSubscription`. Rebinding the plan it already runs is a no-op replay; binding a different plan closes the open period `upgrade` or `downgrade` by the plans' `sortOrder` and opens a fresh binding whose period starts now. `stripeSubscriptionId` attaches the Stripe subscription the `invoice.paid`/`customer.subscription.deleted` events key on and must be unique across bindings (`CONFLICT`). An unknown plan is `NOT_FOUND`, a disabled one `VALIDATION_ERROR`.
- `DELETE /api/v1/admin/organizations/{organizationId}/subscription` — cancel the active binding: the subscription flips `cancelled`, its open period closes `cancelled`, and the organization's next gateway request meters the wallet alone (draws already opened still settle). No active binding is `NOT_FOUND`.
- `GET /api/v1/admin/holds` — every unsettled hold across all organizations, newest first: the in-flight requests the platform is holding money for, read off the sweeper's watch table. Each row also carries the sweep bookkeeping — `sweepAttempts`, `lastError`, `deadAt` — so a hold whose settlement keeps failing is visibly dead-lettered rather than silently stuck.
- `GET /api/v1/admin/requests?organizationId=&keyId=&model=&channel=&from=&to=&status=&minCharge=&limit=&cursor=` — the operator's all-organizations read of the same request log the member endpoint serves (issue #505). Every row carries `organizationId` naming which organization it billed — the field the member read elides because it can only ever name theirs — and `organizationId=` narrows the scan to one organization. The row shape, the `settled`/`errored`/`refused` status set and the `<unix nanos>:<request_id>` cursor are the member endpoint's, so the operator narrows a billing dispute the way the customer does.
- `GET /api/v1/admin/operations?organizationId=&state=&limit=&cursor=` — the operator's all-organizations read of the same `request_operations` the member endpoint serves (issue #601): one durable row per billable turn across every organization, newest first, `organizationId=` narrowing to one. Every row carries `organizationId` naming which organization it billed — the field the member read elides — so the query "every operation stuck `running` past its lease" runs here. The row shape, the state enum and the `<unix nanos>:<operation_id>` cursor are the member endpoint's.
- `POST /api/v1/admin/redemption-codes` — mint a batch of redemption codes: `{"count", "amountMinor", "expiresAt"?}` (`count` ≤ 1000, `expiresAt` RFC 3339 and in the future, absent means the codes never expire). Answers `{batchId, count, amountMinor, expiresAt, codes}` — the only place the codes are readable: the table keeps SHA-256 only. `VALIDATION_ERROR` for a bad count, a non-positive amount or a past expiry.
- `GET /api/v1/admin/anomalies` — the settled turns that did not price cleanly (`capped`, `estimated`, `client_cancelled`, `swept`, `unpriced`), across all organizations and newest first. The rows are read back out of each organization's own ledger — the same settlement records a bill proves — so channel, model, price version, token counts, `upstreamAttempts`, charge and freeze are exactly what was written; a settlement recorded without a settlement record (a hand-driven `Wallet::settle`) does not appear. Each row carries `score` (0–100) and `severity` (`low` < 40, `medium` < 70, `high` ≥ 70), derived at read time from the kind's base weighted by the charge-vs-freeze ratio, the upstream-cost shortfall, the failover count and the request id's repeat-offender count (issue #263); `?severity=` filters to one band and `?sort=score` orders by score descending. Scoring is read-only — it never changes what was charged or written, so an unparseable filter is `400`, not an empty list.
- `GET /api/v1/admin/margin` — what the platform charged versus what upstream cost it, summed per `channel` and `model` over the usage records: `turns`, `chargedMinor`, `upstreamCostMinor`, `marginMinor` (the difference), `providerBilledMinor` and `untrackedTurns`. `untrackedTurns` counts the rows whose upstream cost is NULL — a price with no `upstream` block, a swept turn, or history predating the column — which is deliberately not zero: a coverage gap shows up there instead of inflating the margin. `providerBilledMinor` (issue #283) is what the provider actually billed the channel — the `upstream-bill-sync` pass's billed fact against `upstreamCostMinor`'s estimate — channel-scoped, so the same value repeats on each of that channel's model rows, and `null` while no bill pass has reconciled the channel. Each usage row also carries a `costBasis` (`real_attempt`/`cache_reuse`/`estimate`/`missing`/`provider_reconciled`, issue #604) naming how its `upstreamCostMinor` was derived — the allocation pass reads it rather than re-deriving provenance from `kind` and a NULL figure. Operator-only; upstream cost never reaches an organization-facing endpoint.
- `GET /api/v1/admin/deposits?organizationId=&limit=` — the operator's money-in view (issue #354): the newest deposit rows platform-wide or for one organization, each with its rail, status (`pending`/`confirmed`/`credited`/`reversed`/`expired`), the amount it was opened for, what the rail reported paid (`receivedMinor`, when it differs), the rail's `paymentRef`, the ledger entry once credited, the reversal's `reversalEntryId`/`reversedAt` once reversed, and — for a rail that moves money back — `refundStatus` (`unknown`/`pending`/`succeeded`/`failed`, `null` when none was issued) and `disputedAt` once a dispute opened. The list itself is read-only — reversing is the action below. An unknown `organizationId` is `404`, not an empty list that reads like "never paid".
- `POST /api/v1/admin/deposits/{depositId}/reverse` — a refund: the ledger reversal plus the rail's own refund when it offers one (issues #356, #360, decision D12). Body `{"reason"}` (required — it becomes the reversal entry's description). Reverses one `credited` deposit: posts the mirror entry (Wallet debited, Cash credited) under a key derived from the deposit id and flips the row to `reversed` naming `reversalEntryId`/`reversedAt`, then enqueues `deposit.reversed` in the same transaction. One reversal per deposit ever — a retried call or double-click answers the already-`reversed` row rather than posting twice. `INSUFFICIENT_FUNDS` (402) when the purchased pool cannot cover the debit — the customer already spent the credit; `CONFLICT` (409) for a deposit that never credited (`pending`, `confirmed`, `expired`); `NOT_FOUND` for one that does not exist; `VALIDATION_ERROR` for an empty reason. A `hyperswitch` reversal atomically queues recovery under its original merchant/version and immutable body request ID. Bounded read/create/read recovery starts after commit and spends at most five leased attempts; HTTP errors retain `unknown`, a matching handle reports `pending`, and only exact signed events establish `succeeded`/`failed`. Exhaustion preserves the unresolved state and late-event acceptance. Replayed reversals retain the original reason/entry/request identity. Historical handles are read-only; missing bindings/handles require reconciliation and cannot authorize another create. A rail with no refund path answers `refundStatus: null`.
- `GET /api/v1/admin/reconciliation` — the drift between the ledgers and the tables that project them, answered as `{checkedAt, organizations, clean, classes}`: one entry per drift class (`usage_orphans`, `settlements_unrecorded`, `deposits_unbooked`, `deposits_mismatched`, `deposits_stuck`, `watches_orphaned`, `holds_unwatched`, `holds_dead_lettered`, `log_gaps`, `jobs_dead`, `upstream_price_drift`), each with a `count` and a bounded `samples` list of `{organization, detail}` identifiers. `upstream_price_drift` is platform drift — a stored `upstream` rate the channel's live catalog contradicts, opened by the `sync-upstream-prices` job and self-resolving when the two rates agree again; its samples name `channel:model:field`. The scan is read-only — drift is reported for an operator to act on, never patched in place.
- `GET /api/v1/admin/closings` — every sealed month across all organizations, newest first: the closing records. A seal names the period, how many entries it covers, the log's tree head at sealing, the closing trial-balance root and the seal's own hash, which chains onto the seal before it.
- `POST /api/v1/admin/closings` — queue a month-close. Body `{"month": "YYYY-MM"}`; the month must have fully ended (a malformed or still-running month is `VALIDATION_ERROR` at the request edge, never a queued job that dead-letters in the worker). The close runs as the `close-month` job off the request path (issue #1096), so a deployment whose close outlives the request timeout still seals fully — the answer is the queued run (`{jobId, month, status}`: `queued`, or `already-queued` when a run for that same month is still pending or running, naming its `jobId`), not the seals. One close runs at a time: while a run for a *different* month is pending or running the call answers `409 CONFLICT` (issue #1090). `GET` the closings endpoint for the resulting records; the job endpoint below for the run's progress. Re-queuing a month seals only what an earlier partial run left. Once sealed, no entry may carry a booking date in that month again — the ledger's sealed watermark refuses it at seal time.
- `GET /api/v1/admin/closings/jobs/{jobId}` — one queued month-close's progress, as the console polls it (issue #1090): `status` (`pending` until claimed and again between attempts, `running`, `done`, `dead` after five failed attempts), `attempts`, the latest attempt's `organizations`/`processed`/`sealed`/`failed` counts, its first fifty `failures` (each `{organizationId, name, reason}` — a reason is the ledger calendar's refusal or a validation message; any other failure answers a fixed generic reason and the server log carries the detail), `createdAt` and `finishedAt`. A pass that seals at least one organization finishes `done` even with failures; queue the month again once the cause is fixed. An id that names no close-month run — unknown, malformed, another job kind's, or removed by the jobs retention window — answers `404`, so the path tells a caller nothing about other jobs.
- `GET /api/v1/admin/statements` — every statement the platform has generated, newest period first, drafts and finalized alike; `?period=` and `?organizationId=` narrow it. A malformed or still-running period is `VALIDATION_ERROR`, so a mistyped filter does not look like "no statements". The lazy overdue flip runs first, so a pending statement past its due date already answers `overdue`.
- `POST /api/v1/admin/statements` — generate the period's draft statements: body `{"period": "YYYY-MM"}`, plus `organizationId` to bill one organization only. A month still running or malformed is `VALIDATION_ERROR`. Generation is idempotent per organization and period: a draft rebuilds its lines from the usage rows each call, a finalized statement stands, and an organization with no usage in the period produces none.
- `GET /api/v1/admin/statements/{statementId}` — one statement with its itemized lines, in whichever standing it holds; `NOT_FOUND` for an unknown id. A finalized statement answers the same `anchor` object the organization's own detail serves (the marker's `entryId`/`logIndex`, `windowHash`, `rollupHash`, `origin`, and the signing fields when the deployment signs heads).
- `GET /api/v1/admin/statements/{statementId}/export.pdf` — the admin view of the organization's export: the same finalized statement rendered as a PDF, for any organization. A draft or a missing id is `NOT_FOUND` — an unissued document does not exist to export. Binary `application/pdf`, not the JSON envelope.
- `POST /api/v1/admin/statements/{statementId}/finalize` — issue a draft: locks the lines and totals, snapshots the organization's `paymentTermsDays` into `dueDate`, pins the ledger window (`logFromIndex`/`logToIndex`) the lines prove, and appends the anchor marker to the organization's ledger (issue #221) — an entry whose document hash is the window digest and whose description carries the rollup digest, binding the window fold and the statement's figures. Idempotent — an already-final statement answers itself, appending nothing. The body carries `{"idempotencyKey"}` for uniformity; the transition derives its own idempotency.
- `POST /api/v1/admin/statements/{statementId}/payments` — record verified money: `{"amountMinor", "idempotencyKey"}`. One transaction checks the unpaid credit draws through the issued month, posts `debit Cash / credit CreditLine`, reconciles FIFO standings/events and records source-bound audit. Earlier unbilled draws still take their share; later debt cannot raise this cap. A draft, nonpositive amount or overpayment is `400`. No payment excess funds the wallet. The same key, statement and amount replay the original entry even after full payment; changed source/amount is `409`. An existing ledger entry without matching original audit is refused for explicit reconciliation, never posted again. The response includes current statement standing.
- `POST /api/v1/admin/statements/{statementId}/suspend` — mark an unpaid finalized statement `suspended` (`{"idempotencyKey"}`): the standing for a bill unpaid past grace. Suspension is a standing, not a refusal of money — a later payment still settles it to `paid`. A draft or a paid statement is `400`; suspending an already-suspended one answers the standing document.
- `POST /api/v1/admin/users/{userId}/password-reset` — set a new password for a user (`{"newPassword"}`, same length rule as signup) and revoke every session they hold, in one transaction. Answers `{"userId", "sessionsRevoked"}`; an unknown user is `NOT_FOUND`. This is the only password-recovery path v1 has — no email is sent.
- `GET /api/v1/admin/tiers` — every tier profile, name first, each with its member count (`organizations`). A tier is a capability package — `requestsPerMinute` (one rolling-minute window shared by every key of the assigned organizations) and `modelAllowlist` (a miss is `403` at admission) — never a pricing input; an organization without a tier is unconstrained.
- `PUT /api/v1/admin/tiers/{tier}` — create or replace a tier profile; the name is a lowercase slug. The write is whole-package: absent fields clear their old values, so replaying the same body is the same row and the endpoint carries no idempotency key. `VALIDATION_ERROR` for a malformed name, a non-positive allowance or a malformed allowlist.
- `DELETE /api/v1/admin/tiers/{tier}` — retire a profile. `CONFLICT` while organizations still carry it — a deleted tier never silently uncaps its members; clear the assignments first — and `NOT_FOUND` for a name that was never a tier.
- `GET /api/v1/admin/discounts` — every discount, newest first, ended or not: `{discountId, organizationId?, organization?, model?, percent, label?, validFrom, validUntil?, createdAt}`. The list is the history a settled bill's `discountPercent` points back at.
- `POST /api/v1/admin/discounts` — grant a percent off the priced sum: `{"percent", "organizationId"?, "model"?, "label"?, "validFrom"?, "validUntil"?, "idempotencyKey"}`. `percent` is 1..=100 (a hundred settles at zero); absent `organizationId`/`model` scope to every organization/model; the window defaults `validFrom` to now and is open-ended without `validUntil`. One discount applies per turn — the most favorable matching row, never stacked — resolved when the turn starts and snapshotted into the settlement's `discountPercent`. `VALIDATION_ERROR` for an out-of-range percent, a window ending before it starts or an organization that does not exist; replaying the key with identical fields answers the created row, under different fields it is `409`.
- `DELETE /api/v1/admin/discounts/{discountId}` — end a discount early: `validUntil` becomes now, so turns starting after the call no longer qualify. The row stays in the list; re-ending an already-ended row answers it unchanged, and an unknown id is `NOT_FOUND`.
- `GET /api/v1/admin/service-credentials` — every service credential the deployment has minted, newest first: `{credentialId, name?, prefix, createdAt, revokedAt?}`. The list is the inventory the audit log's `service_credential.*` entries point back at; no secret is ever readable after the mint.
- `POST /api/v1/admin/service-credentials` — retired with 403 before body parsing. Migration 0077 revokes legacy global credentials; new issuance requires the business owner and explicit integration scopes/customers. The deployment list and revoke endpoints retain legacy audit/recovery metadata only.
- `DELETE /api/v1/admin/service-credentials/{credentialId}` — legacy recovery metadata only. All legacy credentials are already disabled by migration 0077. A revoked or unknown id answers `NOT_FOUND`; the list records revocation state. Historical `service_credential.mint` and `service_credential.revoke` audit entries remain intact.
- `GET /api/v1/admin/audit` — the audit log of this surface, newest first: every mutating admin call appends one `oxsum.admin_audit` row, answered as `{entries, nextCursor?}` where an entry is `{auditId, recordedAt, actor, action, target?, detail, idempotencyKey?}`. `action` is the dotted call name (`channel.set`, `discount.create`, `statement.finalize`, …), `target` what it acted on, `detail` the request fields safe to keep — a channel's `apiKey`, a new password and minted codes are never recorded — and `actor` is `operator` today. Query: `limit` (1..=100, default 100), `cursor` echoes a previous answer's `nextCursor` verbatim, `action` narrows to one exact action and an unknown one is `VALIDATION_ERROR`. A keyed write's replay audits once — the `(action, idempotencyKey)` pair dedupes it.

- `GET /metrics` — the process's telemetry in Prometheus exposition format: HTTP request counts and durations by route pattern, the gateway's money path (holds taken, settlements by kind, charged minor units, settled tokens, upstream response latency), rate-limit rejections, idempotency claim outcomes, and scrape-time gauges for open holds and the connection pool. It sits outside the `/api/v1` prefix like `/healthz` but carries the same operator token: a scrape config sends it as `bearer_token`.

`GET /api/v1/admin/diagnostics` is operator-only and read-only: fixed configuration flags, build/schema identity and aggregate provider/merchant/recovery counts. Incompatible/unavailable/unmigrated storage returns `counts: null`, never invented zeroes. One repeatable-read snapshot has a five-second statement timeout and six-second endpoint deadline. No IDs, names, addresses, amounts, payloads, secret values or raw errors are returned, and no providers are probed. The [diagnostic guide](operator-diagnostics.md) defines each count and the sanitized collector.

A deployment that sets no `OXSUM_ADMIN_TOKEN` has no admin surface: the routes exist and answer `UNAUTHORIZED`, rather than being open or absent — `/metrics` included.

## Tree heads (`/api/v1/log`)

Each organization's ledger is its own append-only Merkle log, and the operator signs its head. A user holding an old head can verify the new head was appended onto it — the signature, then a consistency proof:

- `GET /api/v1/log/head` — the current head, signed: a C2SP signed-note (`note`: body, blank line, signature lines), the `origin` (`oxsum/ledgers/<tenant_id>`), the structured head (`size`, lowercase-hex `root`), and the signing key (`keyName`, base64 `publicKey`, the `keyHash` selector naming the operator's signature line). The note text is what the signature covers, byte for byte: verify what you read, not what you re-render.
- `GET /api/v1/log/consistency?from=<size>` — the signed new head, the old head at `from` recomputed from the log, and the `proof` (`oldSize`, `newSize`, lowercase-hex `path`) between them. `from` must be at least 1: every log extends the empty tree, so a proof from size 0 would verify against any history and is `VALIDATION_ERROR` rather than answered; `from` beyond the log is `VALIDATION_ERROR` too.
- `GET /api/v1/log/checkpoint` — the last externally anchored head (issue #231): `headSize`, the operator-signed `note`, `updatedAt`, and `cosignatures[]` — one `{witnessUrl, note, firstSeen}` per witness that cosigned this head, exactly as `OXSUM_WITNESS_URLS` names them. `witnessKeys[]` (issue #259) carries the deployment's pinned keyset — one `{witnessUrl, keyName, publicKey, keyHash}` per `OXSUM_WITNESS_KEYS` entry — which `cosignature/v1` key a signature line under each URL must verify against; a verifier that does not trust the deployment's pinset substitutes its own. `data` is `null` before the first anchor or when no witnesses are configured. A third party verifies the operator's signature line against `GET /api/v1/log/key`, each witness's signature line against the pinned (or its own) keys, and its own held head against `GET /api/v1/log/consistency`.
- `GET /api/v1/log/key` — the operator's verifying key. No credential: this is a public key. Fetching it from the server is convenience — a verifier must have chosen the key through a channel the operator does not control, or the signature proves only that the server agrees with itself.
- `GET /api/v1/log/price-head?channel=<name>&model=<name>[&version=<n>]` — the operator-signed checkpoint of one channel-model price row (issue #204): `{note, origin, channel, model, version, rowHash, keyName, publicKey, keyHash}`. The note reuses the `tlog-checkpoint` shape — `origin` is `oxsum/price-catalog/<channel>`, the price version rides in `size`, and `root` carries the hash of the canonical row bytes (`{"v":1,"channel","model","version","weight","price"}` under the `oxsum/price-catalog/v1` domain). Absent `version` serves the latest; a bill names an older one explicitly. Signing is stateless and deterministic, so a stored row signs identically whenever asked — `version=1` still verifies after v2 lands. `NOT_FOUND` for an unwritten version, an unpriced model or an unknown channel; `model` is required, so a channel-only query is `VALIDATION_ERROR`.
- `GET /api/v1/log/sale-head?digest=<hex>` or `?organizationId=<uuid>&model=<name>[&at=<rfc3339>]` — the operator-signed checkpoint of one resolved customer sale snapshot (issue #600): `{note, origin, businessId, organizationId, model, version, digest, rowHash, keyName, publicKey, keyHash}`. The note is the same `tlog-checkpoint` shape — `origin` is `oxsum/sale-catalog/<businessId>`, the snapshot's book `version` rides in `size` (0 when it names none), and `root` carries the digest of the canonical snapshot bytes (`{"v":1,"businessId","organizationId","model","source","scheduleId","bookId","version","draftRevision","legacyChannel","effectiveFrom","effectiveUntil","terms"}` under the `oxsum/sale-catalog/v1` domain) — the customer-visible terms only, never supplier rates. `digest` selects the stored snapshot a settlement committed; without it, `organizationId`+`model`+`at` re-resolve the terms in force. Signing is stateless and deterministic over the stored canonical row, so a committed digest re-signs identically whenever asked. A customer reads only its own sale terms: `organizationId` must be the caller's organization and a digest resolving to another customer's terms is `NOT_FOUND`. `NOT_FOUND` for an unrecorded digest or no active terms; `VALIDATION_ERROR` when neither `digest` nor `organizationId`+`model` is given.

The key signs under the fixed name `oxsum/tree-heads`; the seed is `OXSUM_HEAD_SIGNING_KEY` (32 bytes, base64). A deployment that sets none serves the wallet but not the log surface: those routes answer `SERVICE_UNAVAILABLE`.

### Serving as a witness (issue #260)

A deployment that sets `OXSUM_WITNESS_SIGNING_KEY` runs its own `tlog-witness` beside the API: other logs' operators pin it as one of their `OXSUM_WITNESS_URLS`, and their `witness-checkpoints` passes submit here.

- `POST /add-checkpoint` — root-level, `text/plain` both ways, per the C2SP spec. The request is the submitter's signed checkpoint note, a blank line, then one base64 line per consistency-proof hash; the answer on accept is `text/plain` — the same note plus this witness's `cosignature/v1` line (key name `OXSUM_WITNESS_KEY_NAME`, default `oxsum/witness`; public key derived from `OXSUM_WITNESS_SIGNING_KEY`). Refusals: 400 malformed body or a proof that is absent/does not verify, 401 a note carrying no signature line, 404 an origin this witness does not track (every tracked origin is pinned in `OXSUM_WITNESS_TRUST` — there is no trust-on-first-use), 409 a head that contradicts the one already vouched — same size different root, or shrunk behind it. A re-offer of the head the witness already vouched is a 200 with a fresh cosignature, never an error. The accepted head and the returned note land in `oxsum.witness_served`/`oxsum.witness_vouched` in the same transaction the response commits from — durable before it is signed.

## Response format

Success:

```json
{ "data": {} }
```

Failure:

```json
{ "error": { "code": "VALIDATION_ERROR", "message": "caller-facing explanation", "details": [] } }
```

Every failure on the envelope's surface is this shape, including a request body the server cannot read: a missing or wrong `content-type`, a body that is not JSON, and JSON that does not match the endpoint's request type are all `VALIDATION_ERROR`, which is the 400 every body-taking path documents. `/v1` is the exception, and its own section says so.

Request types are closed: money-bearing and administrative request bodies deserialize with `deny_unknown_fields` (issue #1080), so a field the endpoint does not declare is `VALIDATION_ERROR` rather than silently ignored — a misspelled amount or a stray key is a caller's mistake surfaced at the boundary, never a field that slipped past validation.

## Error codes

| code | HTTP | meaning |
| --- | --- | --- |
| `VALIDATION_ERROR` | 400 | request validation failed |
| `UNAUTHORIZED` | 401 | credential missing, malformed, unknown, revoked or expired — key, operator token or session alike — or a login with an unknown email or a wrong password |
| `FORBIDDEN` | 403 | the caller may not do this; registration when signup is not open |
| `INSUFFICIENT_FUNDS` | 402 | the wallet cannot cover it: a hold larger than the available balance |
| `KEY_LIMIT_EXCEEDED` | 429 | the acting API key's spend limit is exhausted: settled charges plus outstanding holds attributed to the key would exceed it; `details` carries `[{limitMinor, committedMinor}]` |
| `RATE_LIMITED` | 429 | the acting API key's rolling-minute request allowance is used up; `Retry-After` and `X-RateLimit-*` headers say when a slot frees (the same headers `TOKENS_PER_MINUTE` carries for the token-allowance twin) |
| `TOO_MANY_HOLDS` | 429 | the acting API key's outstanding-holds cap is reached: settle or let a hold lapse before opening another; `details` carries `[{limit, open}]` |
| `INVALID_MANDATE` | 403 | a presented spend mandate did not verify against the key's public half — bad base64, bad JSON, or a signature that does not check (issue #262) |
| `MANDATE_NOT_YET_VALID` / `MANDATE_EXPIRED` | 403 | the mandate's `notBefore` is still ahead, or its `notAfter` has passed |
| `MANDATE_EXCEEDED` | 429 | the mandate's `capMinor` is spent: committed usage under it is at or over the ceiling; `details` carries `[{capMinor, committedMinor}]` |
| `POOL_BUSY` | 429 | the shared database pool could not hand out a connection in time, or a keyed hold queued longer than the wait budget behind another hold of the same key (issue #486) — momentary backpressure, not a caller quota; `Retry-After` without the `X-RateLimit-*` headers a caller-owned limit carries (issue #235) |
| `NOT_FOUND` | 404 | resource does not exist, or belongs to another organization; a settlement naming a hold that is not outstanding |
| `CONFLICT` | 409 | the value is already taken, or a key was reused for a different request; registering an email that exists; settling a hold that is already discharged |
| `DAY_SEALED` | 409 | the settlement's booking day is already anchored by the daily usage-anchor pass: the day log's leaf for that day is committed and must not change (issue #340) |
| `INTERNAL_ERROR` | 500 | server error; details only in logs |
| `SERVICE_UNAVAILABLE` | 503 | a feature the deployment did not configure: the wallet works, this surface does not — the tree-head endpoints without `OXSUM_HEAD_SIGNING_KEY`, the Stripe top-up intent without `OXSUM_STRIPE_*` |

## API keys

- Format: `oxs-` followed by 32 random bytes, so secret scanners can recognize one. The plaintext is returned once, at creation; the database stores only its SHA-256 hash and a display prefix.
- Keys belong to an organization and hold no money. `created_by` records the user who minted one, for the role rules product.md defines: a key minted through a session records its creator, a key minted with an API key records none.
- Members see and revoke only the keys they created; owners and admins see and revoke every key of the organization. A key that exists but is not the caller's answers `NOT_FOUND`, never `FORBIDDEN`, so ids cannot be probed. Consumption keys expose only their own metadata and cannot create, change, revoke or delegate credentials. A current personal session is required for all key mutations.
- Revocation is permanent and idempotent; a key id of another organization is `NOT_FOUND`, never `FORBIDDEN`, so ids cannot be probed.
- Expiry is optional. An expired key is refused exactly like a revoked one.
- Since web login (TODO B-4, issue #17), role checks are enforced: members create and revoke only the keys they created; owners and admins see and revoke every key of the organization. Consumption keys have only their own record scope and gateway authority.
- A key may carry a spend limit (`spendLimitMinor`, minor units; null is unlimited), set at creation or with `PATCH /api/v1/org/keys/{keyId}` under the revoke scope rules. The limit caps the key's committed spend — settled charges plus outstanding holds attributed to it. A hold that would push past it is refused with 429 `KEY_LIMIT_EXCEEDED`, atomically with the ledger append, so concurrent requests cannot exceed it. The gateway answers the same refusal as `insufficient_quota` in its OpenAI error shape. Sessions carry no limit.
- `budgetDuration` (`"daily"`, `"weekly"`, `"monthly"`, or null) makes the limit periodic instead of cumulative, over the UTC calendar day, the ISO week (starting Monday) or the calendar month. Settled charges count only when their booking date falls in the current window; outstanding holds always count, because a reservation still binds money now. A window without a limit is meaningless and refused with `VALIDATION_ERROR`.
- `modelAllowlist` (array of model names, or null for all) restricts which gateway models the key may call. A `/v1` request naming another model is refused with 403 (`FORBIDDEN` in the oxsum envelope, `permission_error` in the OpenAI shape) before anything is frozen or sent upstream.
- `requestsPerMinute` (integer ≥ 1, or null for uncapped) bounds the requests the key may start inside a rolling minute — on the gateway. Past it the refusal is 429 `RATE_LIMITED` carrying `Retry-After` and the `X-RateLimit-*` headers; on the gateway the same refusal arrives with the `rate_limit_exceeded` type in the OpenAI shape.
- `maxConcurrentHolds` (integer ≥ 1, or null for uncapped) bounds the holds the key may have outstanding at once. A hold over it is refused 429 `TOO_MANY_HOLDS` — on the gateway the `rate_limit_exceeded` type carries the same code — and a settlement frees the slot it took, because the count includes monetary pending entries and explicit quota-funded/free reservations once each.
- `tokensPerMinute` (integer ≥ 1, or null for uncapped, issue #203) bounds the settled input+output tokens the key may burn inside a trailing minute — read from the durable `oxsum.tpm_windows` table a settlement feeds, never from a hold. On the gateway a request is refused when the tokens already settled inside the minute plus this request's estimate (the input bound plus the output bound, the same figures the freeze prices) exceed the cap: 429 `TOKENS_PER_MINUTE` under the `rate_limit_exceeded` type, with `Retry-After` and `X-RateLimit-*`.
- `tokensPerMinuteScopes` (object mapping a `metadata` tag key to one scope, or null, issue #209) slices the token allowance by tag: `{"team": {"each": 5000}}` gives each `team` value its own 5000/min bucket, `{"tier": {"concrete": "free", "tokensPerMinute": 1000}}` caps only the `tier=free` turns at 1000, and `{"region": {"total": 20000}}` caps every `region` value in one shared 20000 bucket. Each bucket a turn's tags land it in is checked `window-actual + estimate ≤ cap` on the gateway, refused 429 `TOKENS_PER_MINUTE` when full — a full `each` bucket refuses only that value's turns while the rest stay open — and an untagged turn accrues into the plain window alone. An amount-only hold carries no tags, so it can be refused only by a `total` bucket already full.
- `requiredPolicies` (array of guardrail policy names, or null, issue #502) requires the named policies to run on every turn the key makes. Every name must already exist as a configured `guardrail_policies` row — a `VALIDATION_ERROR` otherwise, because a key cannot demand a rule that was never written — and the turn still runs whatever its channel binds on top. A mandate may only *narrow* the list: `requiredPolicies` on `POST /api/v1/org/keys/{keyId}/mandates` must be a subset of the key's own, never an addition.
- `groupId` (a request-group's uuid, or null, issue #508) scopes which of the organization's channels the key's gateway requests may route through: a grouped key's turn resolves only the channels its group names — the candidate set `route_order` walks is filtered at admission, on the requested model and on every configured fallback alike — while a null assignment leaves the key ungrouped and reaching every channel (the backward-compatible default, and what every key minted before groups landed is). A `groupId` that is not one of the organization's own groups is `VALIDATION_ERROR` at mint or patch, never a silent scope to a foreign set. The platform admin maintains the groups themselves: `GET /api/v1/admin/groups?organizationId=` lists an organization's groups with their channel sets, `PUT /api/v1/admin/organizations/{organizationId}/groups/{name}` creates or replaces a group's whole channel set idempotently (`{channelIds}`, each id checked to be a real channel — a group referencing nothing servable would scope keys into silence), and `DELETE` on the same path drops it — an assigned group returns 409 and never implicitly ungroups keys.
- A `PATCH` to `/api/v1/org/keys/{keyId}` replaces the whole constraint set — `spendLimitMinor`, `budgetDuration`, `modelAllowlist`, `requestsPerMinute`, `maxConcurrentHolds`, `tokensPerMinute`, `tokensPerMinuteScopes`, `requiredPolicies`, `groupId` — under the revoke scope rules; each ordinary constraint clears with `null` or omission. `groupId` differs: omission preserves the assignment, explicit `null` ungroups. Consumption keys cannot create or patch keys; changing `groupId` requires a current owner/admin session. These checks and key writes commit atomically.
- A current personal session may delegate a strictly narrower slice of a managed key as a **signed spend mandate** (issue #262): `POST /api/v1/org/keys/{keyId}/mandates` mints one under the caller's `key_scope` (a member only on the keys they created; the mint 404s on a key id outside it, so ids cannot be probed). The body is `{capMinor, models?, requiredPolicies?, notBefore?, notAfter}` — every bound must sit inside the key's own (`capMinor ≤ spendLimitMinor`, `models ⊆ modelAllowlist`, `requiredPolicies ⊆ requiredPolicies`, `notAfter` after `notBefore`); a wider ask is `VALIDATION_ERROR`, never minted. The answer carries the mandate plus `mandateBlob` — base64url of the canonical body and its Ed25519 signature — exactly once; the delegate presents it in `X-Oxsum-Mandate` on `/v1/*`. `GET` on the same path lists the key's mandates, metadata only, newest first. There is no revocation — mandates are short-lived by `notAfter`, and a key that must stop delegating re-times its constraints or is revoked. A presented blob that fails any of decode/parse/verify is `INVALID_MANDATE` (403, `permission_error` on `/v1`); the window refuses `MANDATE_NOT_YET_VALID`/`MANDATE_EXPIRED` (403) and a hold over the cap `MANDATE_EXCEEDED` (429), all inside `hold_for_key`'s per-key lock where the mandate's committed sum — its `usage_records` plus `open_holds` — is read. A settled mandate turn's v7 description names `{id, hash}` so a verified bill proves which delegation authorized it.
- `GET /api/v1/org/keys/{keyId}/entitlement` — the "can I" read (issue #501): the same checks admission runs, answered without spending a request slot, a window slot or a hold. `?model=` names the model the request would call (absent skips the allowlist question), `?estimate=` is a hypothetical charge in minor units judged against the spend caps, and a repeated `?tag=name:value` resolves which tag-scoped token buckets the request would land in. The answer is `allowed` plus a `reason` when refused — `key_inactive`, `org_suspended`, `model_not_allowed`, `rpm_exceeded`, `tpm_exceeded`, `budget_exhausted`, `concurrent_holds`, `mandate_invalid`, `mandate_window`, `mandate_model`, `mandate_cap` — the headroom each constraint leaves (`remainingRpm`, `remainingTpm`, `remainingBudgetMinor`, `concurrentHolds`, `retryAfterSecs`), and a per-constraint `checks` object so a debugging surface shows everything failing, not just the first. The spend-cap verdict folds the key's own `spendLimitMinor` with the minting member's and team's caps — `remainingBudgetMinor` is the tightest one's headroom; the token window judges `settled < cap`, not the estimate (a money figure, not tokens); and a presented `X-Oxsum-Mandate` adds the delegation's own verdict — a blob that cannot verify is the `mandate_invalid` *answer*, not an error, because diagnosing a refused delegate is half of what the endpoint is for. The scope rules are the revoke's — a member reads only keys they created — and the read is a snapshot: a racing request may spend the headroom it reports, which is why the atomic admission checks stay authoritative. It is itself free and never rate-limited.

## Organizations of a user

A user belongs to one organization per membership, and a session acts as exactly one of them — the one the session row names, the oldest membership until it is switched. These three endpoints are a person's actions: they take the session cookie only and refuse a bearer API key, because a key belongs to one organization and names no user — the same rule membership management follows.

- `GET /api/v1/orgs` — every organization the session's user is a member of, oldest membership first, each as `{"organization": Organization, "role": "owner"|"admin"|"member"}`. This is the list the dashboard's switcher offers.
- `POST /api/v1/orgs` — create a `team` organization. Body `{"name": "…"}`, trimmed, 1–80 characters; the user becomes its owner. There is no `personal` kind to create — a personal organization is exactly what signup makes. The session keeps acting as the organization it had.
- `POST /api/v1/session/organization` — switch the acting organization. Body `{"organizationId": "…"}` naming a membership; anything else is `NOT_FOUND`, so the answer never says whether an organization exists. The session row is updated: every request after this one, and every reload, acts as the chosen organization. Answers the `SessionInfo` now in force.
- `DELETE /api/v1/users/me` — delete the session user's account, irreversibly (issue #202). Session-only like the other person-actions; body `{"password": "…"}` is the act's confirmation and a wrong one is `UNAUTHORIZED`. In one transaction the user row is tombstoned (`email` → `deleted-<id>@deleted.invalid`, the password hash blanked, `emailVerified` cleared), every session and every key the user minted (`created_by` — a key minted by a key has no owner) is revoked, the user's memberships are removed, the usage attribution their keys paid (`end_user`, `tags`, on `usage_records` and `open_holds` alike) is nulled, and the OAuth links and email tokens go with the account. An organization the user is the only owner of *with other members still in it* is a `CONFLICT` — transfer ownership first — while an organization whose only member was the account (the personal org every signup makes, or a team org that shrank to one) is tombstoned: renamed `deleted-<id>` and suspended. The ledger is untouched, so the deleted user's bills still verify. Answers `{sessionsRevoked, keysRevoked, membershipsRemoved}`; the session that asked dies in the same commit, so this response is the last thing the credential receives.

## Email flows

The verification and password-reset mails, shipped when the deployment configures a mailer (`OXSUM_SMTP_URL`, `OXSUM_MAIL_FROM` and `OXSUM_PUBLIC_URL` together — a subset is a startup error). A token is `oxt-` plus 32 random bytes, stored only as its SHA-256 and named for its purpose — a token minted for one mail serves no other. Consumption is a single statement that checks live-and-unused and stamps the spend, so a token redeems once ever; unknown, spent and expired all answer the same `NOT_FOUND`. A verification link lives seven days, a reset link one hour.

- `POST /api/v1/auth/verify/request` — mail the session user a fresh verification link. Session-only: an API key names no user and is `FORBIDDEN`. Answers `{"sent", "alreadyVerified"}` — `sent: false` when the sixty-second resend cooldown since the last mint has not passed, `alreadyVerified: true` once the address is verified. The one endpoint that reports a deployment without a mailer: 503 `SERVICE_UNAVAILABLE`, because sending the mail is its whole job.
- `POST /api/v1/auth/verify` — consume a verification token. Public: the token is the credential. Body `{"token": "…"}`; answers `{"verified": true}` and sets `emailVerified` on the user, which the session payload carries from then on.
- `POST /api/v1/auth/password/forgot` — mail a reset link when the address has an account. Public, and deliberately indistinguishable: a known and an unknown address both answer an empty 200, and a send failure answers the same — the response never says whether the account exists.
- `POST /api/v1/auth/password/reset` — consume a reset token and set the password. Body `{"token": "…", "password": "…"}`; answers `{"sessionsRevoked"}` — every session the user held dies with the old password, the admin reset's semantics self-served through the mail.

Registration and invitation redemption mail the verification link themselves and say so in `verificationSent`; a send failure there logs and answers `false` rather than failing the request — the account stands, and `verify/request` is the retry.

## OAuth login

OAuth login ships per configured provider (issue #152, generalized in #504): `github` when `OXSUM_GITHUB_CLIENT_ID`/`OXSUM_GITHUB_CLIENT_SECRET`/`OXSUM_PUBLIC_URL` are all set, and `oidc:{name}` per `OXSUM_OIDC_PROVIDERS` entry. These are browser redirects, not JSON: the caller is a browser mid-flow, so every failure answers `303` back to `/app/login?error=oauth` and the detail goes to the logs rather than the query string. The `state` is `oxo-` plus 32 random bytes, stored only as its SHA-256, single-use and ten-minute-lived — the callback's consumption stamps the spend in the same statement that checks it live, so a replayed or raced callback redeems nothing. An OIDC state's row also carries the PKCE `code_verifier` and the `nonce` the authorize leg minted: the token exchange presents the verifier (S256 — the authorize URL carried only its SHA-256), and the `id_token` must echo the nonce, is signature-verified RS256 against the provider's JWKS, and must pass `iss`/`aud`/`exp` validation.

- `GET /api/v1/auth/methods` — which login methods the deployment offers: `{"oauthGithub": bool, "oidcProviders": string[], "turnstileSiteKey": string|null}`. The login page's only unauthenticated read; it draws the GitHub button, one button per configured IdP, and the Turnstile widget from it.
- `GET /api/v1/auth/oauth/{provider}` — the start: `303` to the provider's authorize page carrying the minted state (GitHub's leg carries `scope=read:user user:email`; an OIDC leg carries `scope=openid email profile`, the S256 `code_challenge` and the `nonce`). `404` on an unconfigured or unknown provider.
- `GET /api/v1/auth/oauth/{provider}/callback?code=…&state=…` — the finish: consumes the state, runs the provider's exchange and identity legs (GitHub's access-token plus userinfo; OIDC's `id_token` verified as above), and resolves the account — a linked `oauth_accounts` row first, then a verified-email match that records the link, otherwise a registration when signup is `open`. Success answers `303` to `/app` with the session `Set-Cookie`; the provider's own `error` parameter, a bad state, a failed exchange or verification, and an identity with no verified email all answer `303` to `/app/login?error=oauth`.

An account the callback creates has no known password — the stored hash is argon2 over a random secret nobody holds, so the password path can never open it; a mailed or admin reset sets a real one. Its email lands `emailVerified` because the provider's report already did that work, and the link rows in `oxsum.oauth_accounts` key on `(provider, provider_user_id)` — an OIDC `sub` is only unique per issuer, so `provider = oidc:{name}` keeps each configured IdP's namespace distinct — never the email, which the provider may reassign.

## TOTP second factor

An optional per-user second factor (issue #504): the session user enrolls an authenticator secret (sealed at rest under `OXSUM_SECRET_KEY`, like an upstream credential), confirms it on a live code, and from then on their password login is two legs — the password verifies, then a single-use challenge trades for a live six-digit code or one of the ten single-use backup codes minted at confirmation.

- `POST /api/v1/auth/totp/enroll` — session only (an API key is `FORBIDDEN`). Answers `{secret, otpauthUrl}` — the base32 secret once, and the otpauth URL the client renders a QR from. The secret is unconfirmed: it gates nothing until `confirm` proves a code against it.
- `POST /api/v1/auth/totp/confirm` — session only; `{code}` a live code. Confirms the factor — it gates every password login from then on — and answers `{backupCodes}`: ten `xxxx-xxxx` single-use codes, shown this once and stored only as SHA-256 hashes. A wrong code is 401 and leaves the enrollment pending.
- `POST /api/v1/auth/totp/disable` — session only; `{code}` a live code. Removing the factor is itself second-factored, so a hijacked session alone cannot strip it.
- `POST /api/v1/auth/totp` — the login's second leg, open (the challenge is the credential). `POST /api/v1/auth/login` on a factored account answers `{"requiresTotp": true, "challengeToken"}` with no cookie; this route trades `{challengeToken, code}` — a live six-digit code or a backup code, consumed — for the `SessionInfo` plus `Set-Cookie` a password-only login would have minted. The challenge is single-use, ten-minute-lived and survives five wrong codes; a wrong code is 401, a spent/exhausted/expired one `NOT_FOUND`, so a stolen password lets a probe brute neither the session nor the digits. The session it mints carries `totp_verified_at`, the stamp a freshness-requiring route would re-check (v1 records it; no route gates on it yet).

## Device authorization

The device grant (issue #156) — an RFC 8628-flavored flow for a CLI or any input-constrained tool to receive an API key through a browser session, without a secret crossing a terminal. The tool leg is public, the user leg is session-only.

- `POST /api/v1/device/code` — mints the request: `{deviceCode, userCode, verificationUri, expiresIn, interval}`. The tool keeps the `deviceCode` (`oxd-` plus 32 random bytes, SHA-256 at rest) and shows the `userCode` (`XXXX-XXXX`, unambiguous alphabet) to the user. Both lapse fifteen minutes after minting.
- `GET /api/v1/device/request?code=…` — session only; reads the pending request for the approval page. Unknown, expired and decided codes are all `NOT_FOUND`.
- `POST /api/v1/device/authorize` — session only; `{userCode, approve}` stamps the verdict atomically. Approving records the session's user and current organization — the key mints there, so an approver can only grant into an organization they act in. A raced or spent code is `NOT_FOUND`.
- `POST /api/v1/device/token` — the tool's poll: `{status: "pending"|"denied"|"consumed"}`, or `{status: "approved", apiKey}` on the first poll after approval — the key is minted inside that transaction, so its secret is never at rest. Polling inside `interval` is 429 `SLOW_DOWN` with `Retry-After`; unknown, expired and already-consumed codes are `NOT_FOUND`.

A delivered key is an ordinary organization key — `createdBy` names the approver, listed and revocable like any other.

### Anti-bot check

Cloudflare Turnstile on the two public account-creation endpoints, shipped when the deployment configures `OXSUM_TURNSTILE_SITE_KEY` and `OXSUM_TURNSTILE_SECRET_KEY` together — one of the pair alone is a startup error (issue #154). The page learns the site key from `GET /api/v1/auth/methods`, renders the widget from it, and sends the widget's answer as `turnstileToken` in the registration or redemption body; the server verifies it against `siteverify` before the account is touched. The field is ignored on an unconfigured deployment — it is required only where the check exists — and a missing token or a `success: false` verdict both answer `FORBIDDEN` without hinting which leg failed, while a verifier that cannot be reached answers `SERVICE_UNAVAILABLE`: the check fails closed, never open.

## Membership management

A tenant is an organization and its memberships carry a role (owner, admin, member). Managing them is a *person's* action: these four endpoints require the session cookie and refuse a bearer API key, because a key is not a person and names no role. Consumption keys have no account-management authority. The rules are enforced in `crates/core/src/orgs.rs`, so the members page's server functions refuse exactly what these endpoints refuse.

- `POST /api/v1/org/members` is retired. Authenticated calls return 403 before body parsing; knowing an email cannot attach an account.
- `POST /api/v1/org/invitations` requires a current customer owner/admin session and `idempotencyKey`. Optional `intendedEmail` binds consent without mail; optional `email` binds and requests delivery (both must match if provided). Returns `{id, organizationId, token, expiresAt}`; only first creation returns the secret. Exact retries return token null and do not resend; different terms conflict. Without a mailer, requested delivery is refused before creation.
- `POST /api/v1/invitations/preview` takes `{token}` and returns only the target name, organization ID, offered customer role and expiry. The invitation secret authorizes this limited read; no intended email or credentials are disclosed.
- `POST /api/v1/invitations/accept` takes `{token}` with a live personal session in either context. Bound email must match; a configured second factor must be satisfied. It returns `{id, organizationId, role}` as a historical consent receipt and does not select the customer, mint keys or grant staff privileges. Same-person replay is safe even after membership removal; it does not restore access. Other spent, wrong-email, revoked, expired and unknown tokens are `NOT_FOUND`.
- `POST /api/v1/invitations/redeem` registers a new account using `{token,email,password,turnstileToken?}`. Configured bot verification remains required. Team links grant member in the existing organization; business customer links create the reserved customer and initial customer owner. The first key is shown once. Existing addresses conflict and must log in to accept. Account, profile/membership, key, invitation consumption and audit commit together; replay cannot retrieve the key.
- `POST /api/v1/org/invitations/{invitationId}/revoke` requires current owner/admin, `idempotencyKey` and `reason`. It returns an immutable mutation receipt. Accepted links conflict; revoke the membership separately. Foreign links are `NOT_FOUND`.
- `POST /api/v1/business/customer-invitations` requires selected business `customersWrite`, `idempotencyKey,email,name,externalId,reason`. The seven-day reservation is bound to its intended email and customer UUID. No mail is sent by this endpoint. Same request returns the original invitation with token null. Pending live and assigned external IDs conflict. `/business/customer-invitations/{invitationId}/revoke` accepts `{idempotencyKey,reason}` with the same capability. No deployment/consumption bearer can stand in for a personal session.

- `PATCH /api/v1/org/members/{userId}` — change a member's role and/or spend budget: `{"role"?: "admin"|"member", "budgetLimitMinor"?: int64|null}`, at least one of the two. `owner` is not a value of `role`, so no request body can promote anyone to owner. `budgetLimitMinor` caps the member's committed spend — settled charges plus outstanding holds summed over every API key they minted — enforced inside the hold's own locked window, so two keys of the member cannot together exceed it; absent leaves the cap, explicit `null` clears it, `0` caps spend at nothing, and a negative is `VALIDATION_ERROR`. A key minted by an API key has no creator, and a removed member's keys are unbudgeted: the cap hangs on the membership row. Answers the updated `Member`, which now also carries `budgetLimitMinor`.
- `DELETE /api/v1/org/members/{userId}` — remove the membership. The user and their personal organization are untouched. Answers the `Member` that was removed.
- `POST /api/v1/org/ownership` — transfer ownership. Body `{"userId": "…"}`. The named member becomes `owner` and the transferring owner becomes an `admin`, in one transaction, so the organization has exactly one owner before and after. Answers `{"owner": Member, "previousOwner": Member}`.

### Who may do what

| Acting credential | add | remove | change role | transfer ownership |
| --- | --- | --- | --- | --- |
| Session, `owner` | yes | yes, except the last owner | yes, except the last owner | yes |
| Session, `admin` | yes | only members and admins | only members and admins | no, 403 |
| Session, `member` | no, 403 | no, 403 | no, 403 | no, 403 |
| API key (bearer) | no, 403 | no, 403 | no, 403 | no, 403 |

- **The last owner is protected.** An organization always has at least one owner: the sole owner cannot be removed or demoted (`CONFLICT`), whichever endpoint is asked, so ownership has to be transferred first. `POST /api/v1/org/ownership` moves the seat — it never creates a second owner — and the previous owner stays an admin, so they can still manage members.
- **An admin may not touch an owner.** An admin removing or re-rolling an owner is `FORBIDDEN`, not `CONFLICT`: the refusal is about the admin's authority, not about the owner count.
- **No role change grants ownership.** The only path to `owner` is the ownership endpoint, so an admin cannot promote anyone (including themselves) to owner by any route.
- **The acting role is checked before the target is looked at.** A caller who may not perform an action is refused `FORBIDDEN` whatever they name, so an admin's ownership transfer naming an account that is not even a member answers 403, not 404. Within the allowed roles, naming a non-member is 404.
- **Any member may read the list.** Reading who is in the organization is not a management action, and `GET /api/v1/org/members` is reachable by any live session; the session resolves through `memberships`, so a session can only ever read the list of an organization it is a member of (docs/decisions.md, issue #61).
- Adding the second person to a `personal` organization flips its `kind` to `team`; the ledger does not move and nothing flips back when a member is removed.

### Statuses

| situation | status | code |
| --- | --- | --- |
| not a session in the owner or admin role (member, or any API key) | 403 | `FORBIDDEN` |
| an admin acting on an owner | 403 | `FORBIDDEN` |
| an admin transferring ownership | 403 | `FORBIDDEN` |
| unknown account (the email names nobody) | 404 | `NOT_FOUND` |
| target is not a member of the caller's organization | 404 | `NOT_FOUND` |
| the add email is already a member | 409 | `CONFLICT` |
| the last owner cannot be removed or demoted | 409 | `CONFLICT` |
| ownership transferred to the current owner | 409 | `CONFLICT` |
| a body naming `owner`, or one that is not the request type | 400 | `VALIDATION_ERROR` |

## Pagination

`GET /api/v1/admin/organizations` paginates (issue #93): `?limit=` caps a page at 100 rows (the default), and a page that is not the last answers `nextCursor` — an opaque string the next call echoes back as `?cursor=`; absent means the list is done. A cursor is a keyset bound, never a page/offset: a malformed one answers `400 VALIDATION_ERROR`. The dashboard's own lists paginate the same way — the bills, transaction-log and requests pages walk their ledgers with the log index as `?before=` — and bounded lists (`GET /api/v1/org/keys`, holds, anomalies, closings) answer in full.

## Dashboard reads

The SPA's dashboard pages read these (issue #234); all are session or key scoped to the caller's organization and share the cursor convention:

| endpoint | answers |
| --- | --- |
| `GET /api/v1/org` | the session's organization — including `displayCurrency`, the fiat rendering it declares (null: credits only) |
| `PATCH /api/v1/org` | session-only, owner or admin — `{"displayCurrency": {"code","ratePerCredit","decimals"} | null}` sets or clears the organization's display currency (issue #257); a member session or an API key is `FORBIDDEN` |
| `GET /api/v1/org/members` | `{members}` — every membership with role and budget cap |
| `GET /api/v1/holds` | `{holds}` — the organization's in-flight holds (gateway turns and historical raw holds, read off the sweeper's watch table), newest first |
| `GET /api/v1/entries?before&limit` | `{rows, nextCursor}` — every ledger entry (index, id, description, contentHash) |
| `GET /api/v1/bills?before` | `{rows, nextCursor}` — only money-moving entries (topUp/adjustment/settlement, signed amount) |
| `GET /api/v1/balance` | plus `reservedMinor` and `spentThisMonthMinor` — the frozen total and month spend |
| `GET /api/v1/org/usage-reports` | `{rows}` — every minted end-user usage report's descriptor, newest first (issue #261) |
| `POST /api/v1/org/usage-reports` | mint the signed report for one `endUser` over `[from, to]` — or replay the standing one |
| `GET /api/v1/org/usage-reports/{reportId}` | the frozen bundle: descriptor, marker and per-entry proofs, signed head |
| `POST /api/v1/org/usage-reports/{reportId}/links` | mint a one-time-token share link to one report (issue #300) |
| `POST /api/v1/org/usage-reports/links` | mint a share link to one downstream customer's issued reports |
| `GET /api/v1/org/usage-reports/links` | every link's descriptor — label, expiry, revocation, last use; never a token |
| `DELETE /api/v1/org/usage-reports/links/{linkId}` | revoke a link, idempotently |

One public read sits outside that scope: `GET /api/v1/branding` answers the
effective seller identity `{name, logoUrl, accentColor, contactEmail, helpUrl}`
(#614/#756) with no credential. It resolves the owner-managed business profile,
or setup/deployment defaults before the first save. Nothing it serves changes
pricing, the ledger, currency or historical proofs. See Business identity and
public branding below for precedence, revisions and failure behavior.

## Change process

1. Change `crates/server/openapi.yaml`
2. Change the backend implementation, add integration tests
3. Change the callers (the React dashboard under `web/`)

## Deployment business ownership (#593)

`GET /api/v1/admin/business` returns the singleton business and immutable ownership-migration receipt. `GET /api/v1/admin/business/customers/{organizationId}` returns the customer profile within that business. Both require the deployment token; customer sessions/consumption keys cannot inspect seller administration. Customer wire IDs continue to use `organizationId` and `tenantId` for compatibility. A foreign or unknown ownership pair is not found. These reads do not initialize an owner, change registration policy or move money.

## Initial business owner consent (#629)

Deployment authority calls `POST /api/v1/admin/business/setup-tokens` with `idempotencyKey` and `ownerEmail`. The response includes a token only on its first mint, with a 30-minute expiry. Identical mint retries return the same ID/expiry with `token: null`; changed owner/email conflicts. Treat the token as a secret, pass it directly to the intended owner, and do not put it in URLs or logs. A lost token requires a new mint key while the business is still uninitialized.

An existing user accepts through `POST /api/v1/business/setup` with their personal session cookie and `{token, businessName}`; explicit Bearer credentials are refused. A new owner instead uses `POST /api/v1/business/setup/register` with `{token, businessName, email, password}`. The email must match the invitation; an existing address uses the session flow rather than resetting its password. New-owner registration returns the ordinary customer registration/key once, and the owner logs in normally afterwards. Both acceptance routes return `ownership`; `registration` is null for an existing owner. An initialized business refuses further setup with 409, and invalid/expired invitations refuse with 404. Inspect the deployment business read after a lost acceptance response.

## Business session authority (0.102.0, #630)

`GET/POST /api/v1/session/context` reads/selects the personal session's context; POST takes `idempotencyKey`, `context`, and optional `organizationId` for customer selection. Business selection rejects customer IDs and requires a current staff membership. All new business routes reject explicit Authorization credentials, even alongside a valid cookie. Customer routes continue accepting their existing credentials but refuse a session selected into business context. The read's `business` names the session's staff principal in *either* context — the role/capability set the membership carries — so a client can learn the seat is claimable before it is selected; `context` alone says which authority the session is acting under, and a non-staff user reads `business: null` (#615).

`GET /api/v1/business` returns the current business/user/role/capability set. Owner-only routes are `GET/POST /business/staff`, `POST /business/staff/revoke`, `/business/ownership`, and `/business/registration`. Writes take an idempotency key and reason, return a stable mutation receipt, and write actor-aware audit atomically. Staff assignment accepts an existing user UUID and a non-owner role. Transfer promotes an existing staff member and demotes the actor. Replay checks current permission before returning an old receipt, so a former owner cannot use a transfer receipt as continuing authority.

Login and TOTP completion preserve `SessionInfo` for users with a customer membership. A user without one receives `SessionContextInfo` with null `organizationId`, retaining personal identity without customer access. Personal account deletion, email verification request and TOTP settings accept the session in either context. Business access for TOTP-enabled users requires `totp_verified_at`; an OAuth session alone does not satisfy that gate. Setup and deployment recovery remain separate routes.


## Customer profiles and lifecycle (0.103.0, #633)

Business-session `GET /business/customers?after=<organizationUuid>&limit=50` returns `customers` and `nextCursor`, scoped to the selected business and ordered by immutable customer UUID. Limit is 1–100. `GET /business/customers/{organizationId}` needs `customersRead`; unknown/foreign IDs return 404. Viewer has no customer-detail permission. Responses exclude keys, private payloads and ledger schema names.

Owner/admin `PATCH /business/customers/{organizationId}` takes `idempotencyKey`, `name`, `contactEmail` (string or null), and `reason`. External ID, business ID and organization ID are immutable. `POST .../lifecycle` takes the key, `state` (`active`/`archived`) and reason; `POST .../suspension` takes the key, `suspended` boolean and reason and also permits support. All writes recheck core capability and business ownership, return a stable mutation receipt, and atomically audit the change. Replaying an old lifecycle key does not reapply it after a later restore.

Archive, manual suspension and dunning each block new admission. Restore clears only archive; manual reinstatement clears only manual suspension; dunning recovery clears only its own marker. Existing holds settle and payment/refund recovery remains possible. Legacy deployment `suspended` reads report the effective combined state, while writes affect only the manual reason.


## Consumption authority compatibility (#637)

The [customer capability inventory](customer-capabilities.md) is the route review checklist. Consumption keys retain gateway calls, own key metadata/entitlement, own usage/request/hold/bill/raw entry/proof reads, and safe price/head metadata. They cannot read customer balances/budgets, initiate payments, manage keys/mandates/members/teams, export customer-wide history, or manage webhooks/reports/statements/experiments. Existing key IDs, secrets, ledger origins and proof contents stay unchanged; the authority is intentionally narrower.

Response-cache digests bind the exact consumption key before lookup/storage, so sibling keys and pre-upgrade unbound rows miss. Idempotency fingerprints bind key, protocol, path model and body; a mismatching caller never releases or overwrites the original claim. Account-wide marker entries cannot bypass statement/report permission through generic entries/proofs/exports. Member history for expired/revoked keys remains readable by that member's session.

### Customer-authorized model catalog (T07.a, #644)

`GET /v1/models`, `GET /api/v1/pricing`, `POST /api/v1/estimate-price` and direct `GET /api/v1/log/price-head` filter the caller's business/customer tier, plus a consumption key's models and request-group channels. Session catalog reads use the customer tier. Aliases require both the public name and resolved target to be allowed; an alias's own price wins, otherwise its target price applies. Estimates consider only permitted routes. A denied estimate has the same validation error as an unavailable name; denied direct price checkpoints return 404. Existing exported proofs still verify independently.

Gateway primary, experiment, fallback and alias routes share this admission snapshot. Experiments cannot replace a denied original model with an allowed variant, and existing `requestedModel`/`servedModel` evidence preserves both names. Changes affect later admissions; an existing task's read/cancel still uses its original customer/key and endpoint. Generic tasks currently require a direct model and refuse a signed mandate; T13 supplies the complete task operation/price/evidence binding. See the [protocol matrix](roadmap/self-hosted-v1/PROTOCOL-MATRIX.md). Complete cache context is the required T07.b follow-up.


## Customer sale books (`/api/v1/business/sale-books`, #599)

OpenAPI 0.111.0 adds sale configuration and preview, independently of supplier channel costs. Select business context with a current personal session. Owner/admin/finance can read books, versions and customer term history and preview prices. Owner/admin create books, save drafts, publish and schedule defaults; owner/finance assign customer books and override individual models. Support/viewer, customer sessions and every machine credential are refused.

- `GET /sale-books?after=&limit=` returns UUID-paged book metadata. `GET /sale-books/{bookId}` returns immutable draft revisions, publications and book schedule history. `GET /business/customers/{organizationId}/sale-terms` returns that customer's assignment and override histories.
- `POST /sale-books/changes` accepts `{idempotencyKey, reason, change}`. `change.operation` is `createBook`, `saveDraft`, `publishDraft`, `scheduleDefault`, `assignBook`, `overrideModel` or `endSchedule`. `saveDraft` compares `expectedRevision` (zero initially), then appends a revision containing the complete `models` map. `publishDraft` pins the exact saved revision. Its returned `revision` is the new publication version. Publishing does not schedule prices.
- Scheduled defaults and assignments pin `bookId` and published `version`; an override pins `organizationId`, `model` and complete `terms`. Each term contains `price` and `discountPercent` (0–100). Supplier `upstream` fields are refused even in rule price sets. Same-specificity overlapping price rules are refused by the existing pure rater.
- Windows are UTC microseconds, `[effectiveFrom,effectiveUntil)`. Pass `effectiveFrom: null` (or omit it) to activate at transaction time — the intended spelling for "now". Explicit timestamps are truncated to microseconds before comparison and persistence; a start within a few seconds of the server's clock is accepted as the caller's own now (clock-skew tolerance), while a start further in the past is a backdated window and refused. Nonempty new windows in one scope must not overlap. An optional `replaces` atomically ends an existing schedule of the same scope at the new start. `endSchedule` appends one immutable end event, may cancel future terms at their start, and cannot extend the original end or replace an earlier end event. Both the original and effective end are returned in history.
- `POST /business/sale-price-preview` accepts `{organizationId, model, at?, source?, usage}`. The default `source: {kind: "effective"}` resolves a per-model override, the assigned published book, then the default published book. A missing model falls through; a zero rate is a real selected price. There is at most one active assignment per customer and one default. `source: {kind: "draft", bookId, revision}` previews an exact draft. `source: {kind: "legacy", channel, version}` reads an exact legacy price; it never promotes that route to customer-wide sale authority.

The preview returns a self-contained `snapshot`, itemized tuple `lines`, `matchedRule`, `undiscountedMinor` and `chargeMinor`. Rates are integer minor units per million normalized units; a per-request rate is in minor units per request. All priced lines are summed before the single selected percentage discount and one ceiling division. Discounts never stack or consult the legacy discount table. Negative counts, invalid subsets, unknown fields, unsupported positive dimensions and integer overflow fail closed. Absent cache/reasoning rates retain the existing base input/output fallback. Explicit legacy previews remove supplier costs at every rule level while retaining channel/model/version identity.

Mutation, current authority, audit and stable result commit atomically; exact retries return the original result, while changed bodies conflict and revoked roles cannot replay. No new endpoint grants consumption permission, creates ledger entries or calls a provider. T08 supplies configuration and pure snapshots; T09–T13 connect them to signed evidence, admission and settlement. Existing gateway/channel price behavior remains in place until that integration, and existing historical proofs remain unchanged.

### Business merchant configuration (`/api/v1/business/merchants`)

`GET /api/v1/business/checkout-routes` reads the three rail selections under
ReportsRead. `POST` requires owner MerchantSecretsWrite, `idempotencyKey`,
`reason`, `rail`, `expectedRevision` and nullable `merchantId`. A selected merchant
must be enabled and owned by the business on that rail. Null explicitly disables
it. Revision zero uses retained environment bootstrap until an owner selection
exists; disabled explicit selections never fall back. Replay receipts retain the
exact original route snapshot across later edits.

Personal sessions read `GET /api/v1/checkout-options`: available rail/name/mode
and route/merchant/version identities, without API URLs, account IDs or secrets.
Consumption and machine credentials are refused. `POST /api/v1/orders` accepts
optional `expectedCheckout: {revision, merchantId, versionId}`. Selection and
version are rechecked under the same business lock that pins the new order;
changed choices conflict before provider I/O. Existing exact-key orders recover
the original merchant/version and SDK metadata through rotation, disablement and
route changes. A different rail under the same key conflicts. Legacy callback
coordinates stay unchanged. Availability is not external PSP certification.

Use a personal session in business context. `GET /api/v1/business/merchants` returns the current version for each owned merchant, paged by `after` UUID and `limit` (1–100). Owner/admin/finance/viewer have redacted read access. `POST` requires current owner capability and `idempotencyKey`, `reason`, stable `name`, `rail`, `mode`, `accountId` and complete `config`. Creation starts disabled. Hyperswitch requires API/signing secrets plus public `publishableKey`/`sdkUrl`; Airwallex requires API/signing secrets plus `clientId`/`checkoutUrl`; direct Stripe requires API/signing secrets. Each API endpoint has an explicit destination policy; browser URLs require public HTTPS. Recognized test/live key prefixes cannot contradict the declared mode. Saving performs no network call.

`GET /api/v1/business/merchants/{merchantId}/versions` returns immutable redacted history in ascending revision order (`after` revision and `limit`). `POST` on that path requires `idempotencyKey`, `reason` and the full replacement `config`; it appends and selects a version without changing identity or enablement. `POST .../{merchantId}/status` requires the key, reason and `enabled` boolean. Disabling prevents future checkout selection while pinned historical callbacks retain their original context. Mutations return `{id, merchantId, versionId, revision, enabled}`; an exact retry returns its original response after later edits, and a reused key with different content conflicts. Configured credentials, sealed envelopes and client IDs are never read back. These settings alone do not enable the T15/T16 payment-order/customer-checkout workflow.

`GET /api/v1/business/merchants/{merchantId}/events` reads the merchant's scoped intake — see the event conventions above. A deployment can also promote a rail's environment credentials to a merchant at first start: setting `OXSUM_<RAIL>_MERCHANT_ACCOUNT_ID` + `OXSUM_<RAIL>_MERCHANT_MODE` (optionally `OXSUM_<RAIL>_MERCHANT_DESTINATION_POLICY`) bootstraps an explicit merchant for that rail under the business, attributes existing single-rail deposits to it, and routes both checkout and the legacy `/api/v1/webhooks/<rail>` callback through the scoped intake. It runs once per rail and is never overwritten by later environment drift; without it the rail keeps its unscoped legacy behavior but cannot open merchant-bound payments.

## Sold plans and customer entitlements (`/api/v1/plans`, `/api/v1/org/plan`, #610)

A sold plan is a versioned catalogue entry (T19): `priceMinor`, a per-period `quotaMinor` (`null` unmetered), a `periodKind` (`calendarMonth` rolls at the first of the next UTC month — the legacy boundary policy; `anniversary` rolls on the activation day-of-month, clamped to a short month's last day while the anchor sticks — January 31 renews February 28 then March 31) and the opt-in `overageEnabled`. Catalogue edits mint new versions; a version already bound keeps its sold terms to its boundary. A customer binds one version at a time (`customerPlan`), each paid period opens under an explicit authorization (`paymentOrder` for a fulfilled `planPeriod` order, `complimentary` for a free plan's staff grant) — a timer alone never opens an unpaid period (D06). The entitlement reuses the `subscription_draws` machinery: the sold version mirrors into the quota engine as a `resetPeriod: never` plan and `plan_periods` drives the roll, so draws, evidence and `402` behavior are exactly the subscription path's.

- `GET /api/v1/plans` — the published catalogue versions still on sale, session or consumption key (`scopedRead`); `GET /api/v1/plans/{planVersionId}` reads one version's terms — unpublished or retired versions answer `404` on this surface.
- `GET /api/v1/org/plan` — the caller's live `customerPlan` (`pendingPayment`, `active`, `renewalPending`, `pastDue`) plus every authorized `planPeriods` row with its bounds, quota and `authorizationKind`/`authorizationRef`; `plan` is `null` with no live binding. Personal capability.
- `GET /api/v1/org/plan/payment-quote` — personal customer session; returns immutable `terms`, SHA-256 `fingerprint` and optional `existingOrderId`. The exact sold credit fee, customer/version/index/bounds, collection revision and exact fiat ceiling are authoritative. Initial bounds are null because service starts at paid activation. Manual renewal quotes become available at the previous UTC boundary. Resume a standing order; never make a second intent for the same payable period.
- `POST /api/v1/org/plan/cancel` — marks `cancelAtPeriodEnd`: the paid period serves to its end, no pro-rating, no renewal at the boundary.
- `POST /api/v1/org/plan/change` — books `scheduledChange` to another sold version, effective at the next boundary: a priced target's first period opens `renewalPending` and activates on its `planPeriod` order, a free one activates on a complimentary grant.

Staff side (personal session in business context): `GET`/`POST /api/v1/business/plans` needs `catalogWrite` — POST mints a version (absent `planId` opens a plan at version 1, an existing `planId` publishes its next). `POST /api/v1/business/plans/{planVersionId}/retire` stops new binds; running subscriptions keep their terms. `GET /api/v1/business/customers/{organizationId}/plan` is the staff entitlement read (`customersRead`); `PUT` on that path (`customersWrite`) binds — a priced version binds `pendingPayment`, a free one requires the explicit `complimentaryGrant` uuid in the body. A second live bind is `409`; racing binds resolve on the partial unique index. `POST .../plan/cancel` is the staff-side boundary-cancel.

A `planPeriod` payment order's `purposeRef` is the `customerPlanId`; it must carry `expectedPlanQuote` from the current personal quote and match its exact fiat amount/currency. A live plan/index payment is unique. Paid activation validates the order's ownership, purpose, paid timestamp, immutable version, fee and due period, even through direct core calls. No missing-order or underpayment fallback exists. Refund reservations block new activation; pre-existing authorized historical periods retain replay semantics. Legacy unfulfilled orders without snapshots are fenced from checkout/grant and excluded from the bounded automatic queue while remaining visible for reconciliation. Cancellation/change conflicts while a new unactivated payment is unresolved. Its fulfillment activates the plan (`payment_order.plan_period_fulfilled` in the outbox carries `customerPlanId`/`subscriptionId`/`planVersionId`). A `pendingPayment` initial plan has no paid quota. A `pastDue` plan may still draw its ended period's remainder; it serves until a fulfilled order chains the next period or the plan expires.

## Plan fee journals (`/api/v1/org/plan/journals`, `/api/v1/business/journals`, #611)

Every plan period leaves an invoice journal (T20): v2 at paid activation, v1 at close: `invoice_journals` is the per-period bill — `periodIndex`, `state` (`posted` unpaid, `paid` settled, `void` cancelled), `totalMinor` the lines' sum, `settledMinor` what the receivable-clearing legs already covered, `paidOrderId` the order whose payment completed settlement, and `disputedMinor` the contested figure — and `invoice_journal_lines` itemizes it (`baseFee`, `discount`, `promoCredit`; charges positive, credits negative). `usageOverage` remains a historical read value: a new close never charges usage already settled through wallet or credit again. Historical duplicated charges need explicit source-linked correction (#809). Legacy v1 billing is *in arrears*: a fulfilled `planPeriod` order prepays its period (`debit Cash / credit Liabilities:UnearnedPlanFees`), the close recognizes revenue (`debit Assets:Receivables / credit Income:PlanFees`) and reclassifies the prepay into the receivable; what it did not cover stays `posted` — the same fact `pastDue` carries. Lines are append-only (UPDATE/DELETE refused), the journal's terms and identity are trigger-immutable, and the state machine walks `draft → posted → paid | void` with `paid → posted` only under a lost dispute's recharge.

- `GET /api/v1/org/plan/journals` — the caller's own journals, newest page by `after`/`limit` cursor; `GET /api/v1/org/plan/journals/{journalId}` reads one plus its lines. Personal capability — the billing read the plan surface carries.
- `GET /api/v1/business/journals` — the staff cross-organization list under `reportsRead`: `state` narrows the lifecycle (`posted` is the dunning work list), `customerPlanId` scopes to one binding, `periodIndex` to one period. `GET /api/v1/business/journals/{journalId}` is the staff detail.
- `POST /api/v1/business/journals/{journalId}/void` — cancels the journal under `fundsWrite` `{idempotencyKey, reason}`: reversing lines append so the bill nets to zero, the mirroring ledger entry posts (the open receivable reversed on a `posted` journal; the settlement unwound to `Liabilities:RefundPayable` on a `paid` one), a `journal.void` `finance_audit` row records it, and a replayed key answers the standing voided journal.

Corrections append source-linked lines and entries. A confirmed `planPeriod` refund credits the full returned cash and allocates its debit to original deferred fees, earned-fee contra, refund payable or explicit review suspense. Post-close charge and applied-payment figures decrease together, preserving other outstanding debt; pre-close refunds reduce deferred money immediately and are consumed once at close (#802). A lost dispute also records full cash, restoring receivables only against the original collected allocation and retaining excess as review suspense.

Historical duplicate-usage correction (#809) is an explicit local core operation, not an HTTP endpoint. `Db::correct_legacy_plan_usage` takes an exact journal, reason, idempotency key, matching wallet and personal business `FundsWrite` session. The [local command](plan-usage-correction.md) previews by default, verifies original ledger sources and returns `proposed`, `applied` or `refused` with retained evidence. Apply debits duplicate `Income:PlanFees`, credits original outstanding receivables and puts any collected excess in `RefundPayable`; it never credits Cash. Existing journal/statement reads expose the appended usage and payment-allocation lines. Unsupported combined history explicitly refuses and an apply attempt retains the reason in finance audit. `GET /api/v1/business/audit?action=plan_usage.correct` reads these audited outcomes; `action=plan_refund.recover` reads the corresponding historical refund recoveries. Both values are declared in `FinanceAuditEntry.action`. No new mutation endpoint or machine authority is introduced. The immutable funding map fences new refund requests/approvals before provider authorization; an already-confirmed late refund or chargeback retains full cash as explicit review suspense with corrected journal IDs instead of guessing a different period allocation.

## Plan statements and customer credit (`/api/v1/org/statements`, `/api/v1/business/statements`, #612)

A period close writes two documents in one transaction: the accounting journal above, and the customer-visible *plan statement* (T21) — `plan_statements` restates the journal's bill in customer vocabulary (`planFee`, `usage`, `discount`, `refundCredit`, `disputeChargeback`, `paymentApplied`) and carries the payment terms (`dueAt` = `issuedAt + netTermsDays`, default 14 per binding). The lifecycle is `issued → paid | pastDue | void`; `paidMinor` tracks reconciled payments, so `totalMinor − paidMinor` is what is still owed. The period's prepayment reconciles the statement beside the journal — every partial/full allocation has an order-linked `paymentApplied` line. Full payment walks it `paid`; partial coverage leaves `issued` with `paidMinor > 0`. Renewal fees do not settle older debt. Explicit statement repayment is tracked by #777. Statements are trigger-immutable from `issued` on; a correction appends lines or reissues under `replacesStatementId`.

- `GET /api/v1/org/statements` — the caller's own statements, newest page by `after`/`limit` cursor; `GET /api/v1/org/statements/{statementId}` reads one plus its lines. Personal capability.
- `GET /api/v1/business/statements` — the staff cross-organization list under `reportsRead`: `state` narrows the lifecycle (`pastDue` is the dunning work list), `organizationId` scopes to one customer. `GET /api/v1/business/statements/{statementId}` is the staff detail.
- `POST /api/v1/business/statements/{statementId}/release-review` — the staff release-review under `fundsWrite` `{idempotencyKey, action, reason}`: `extendGrace` walks `pastDue → issued` and moves the selected due date to now plus net terms. Other debt past grace retains suspension. `void` atomically reverses the proven matching journal, ledger receivable and statement; Cash stays unchanged and collected funds become `RefundPayable`. Mixed chargeback, reissued or inconsistent sources return `CONFLICT` before effects. A retained historical document-only void can be reconciled by a new explicit request against its original posted journal. The `statement.release` audit records actor, reason, original journal and actual reversal entry (null when no entry exists). The same target/action/reason/key replays the standing result; changed terms return `CONFLICT`. A new explicit request against an already-financially-void journal audits and refreshes debt suspension without a ledger entry; an exact replay remains inert.
- `POST /api/v1/business/statements/{statementId}/release-review` — the staff release-review under `fundsWrite` `{idempotencyKey, action, reason}`: `extendGrace` walks `pastDue → issued` and moves the selected due date to now plus net terms. Other debt past grace retains suspension. `void` atomically reverses the proven matching journal, ledger receivable and statement; Cash stays unchanged and collected funds become `RefundPayable`. Mixed statements require finance to select the original journal through the journal-void route. Live payment intents on affected documents and reissued or inconsistent sources return `CONFLICT` before effects. A retained historical document-only void can be reconciled by a new explicit request against its original posted journal. The `statement.release` audit records actor, reason, original journal and actual reversal entry (null when no entry exists). The same target/action/reason/key replays the standing result; changed terms return `CONFLICT`. A new explicit request against an already-financially-void journal audits and refreshes debt suspension without a ledger entry; an exact replay remains inert.

Overdue handling: the `statement-past-due` job (leader-elected, minute cadence) flips `issued` statements whose `dueAt` passed to `pastDue`, and past the binding's `graceDays` (default 7) stamps `customer_plans.dunning_suspended_at` — the wallet-side admission gate the hold path reads (a suspended plan still serves subscription quota, which is already paid for; new wallet draws refuse). Payment, grace extension and journal/statement void recompute dunning from all positive debt past its effective grace; renewal, entitlement, manual suspension and archive are independent. `credit_limit_minor` on the binding caps the wallet-side overage a plan-bound turn may hold: `admit_subscription_draw` refuses when `outstanding + thisOverage > creditLimitMinor`, where `outstanding` is remaining statement debt, posted journals without live statements and current holds — `NULL` is unlimited and subscription-drawn quota never counts. `GET /api/v1/org/plan` answers `credit` — `{creditLimitMinor, outstandingMinor, nextDueAt}` — beside the binding. Its displayed `outstandingMinor` is remaining debt; admission additionally counts holds. A posted journal hidden by a retained document-only void remains debt using the latest retained due date (or posting time plus the original binding's current configured net terms if no statement exists), and its past-grace debt blocks wallet admission even before the worker stamps a flag.

## Business identity and public branding (#756)

`GET /api/v1/business/profile` requires a current personal business session with
`overviewRead`; `POST` requires `profileWrite` (owner only). Machine credentials
are refused even alongside a session. A write supplies `idempotencyKey`, `reason`,
`expectedRevision` and the complete `branding` object: `name`, `logoUrl`,
`accentColor`, `contactEmail`, `helpUrl`. Every optional field must be present;
null clears it. Unsupported fields, executable URLs, URL user info/control
characters, malformed email and non-hex CSS colors are refused.

A stale revision is 409 without side effects. Exact retry under the same actor
and key returns its immutable `{id, profile}` receipt, even after a later save;
changed reuse is 409. Replays recheck current owner and TOTP authority. Clients
must re-read after success instead of treating an old receipt as the current
profile. Configuration, revision, immutable receipt and `admin_audit` commit
in the same transaction.

The public `/branding` read now resolves from the database. Revision zero uses
the accepted setup name when initialized, otherwise deployment name defaults;
logo/accent remain deployment defaults and contact/help are null. A stored
profile wins wholesale, including explicit nulls. Database failures are not
masked with a different environment identity. No amount, price, history,
currency or conversion changes through these endpoints.


### Authoritative collection terms (#765)

`GET /api/v1/collection-terms` is a scoped customer read. Business staff read
`GET /api/v1/business/collection-terms` under `overviewRead`; only an owner with
`collectionWrite` may POST `{idempotencyKey, reason, expectedRevision, currency:
{code, ratePerCredit}}`. Rates are exact positive decimal strings, at most eight
fractional digits; current provider minor units have two decimal places. The
response carries revision/source, optional currency, historical currency codes,
readiness and update time. Write retries return the immutable mutation receipt;
stale revisions or incompatible historical currency return HTTP 409.

Provider and manual order creates accept optional `expectedCollectionRevision`.
The browser always sends its displayed quote revision. A new order with a stale
quote or wrong currency conflicts before provider I/O; personal display rates
are ignored. A repeated order key returns its original conversion even after
policy changes. Neither configuring terms nor reading them sends a provider
request. Historic mixed-currency data is retained, surfaced and fenced.


### Daily plan accounting and financial clocks (#838)

`SoldPlan`, `PlanPeriod` and `InvoiceJournal` expose immutable `accountingPolicyVersion` (`1` or `2`). Publication creates v2; there is no caller-provided policy override. V2 paid activation produces the invoice/statement immediately. The hourly worker recognizes completed UTC service days; no HTTP caller supplies an accelerated recognition clock.

`FinancialFacts` adds `billedMinor` (net bill), `deferredMinor` (v2 deferred movement) and `discountMinor` (noncash journal discounts/promotions). `earnedMinor` is net recognized fee, distinct from collection. `refundMinor` counts exact proven cash return once, never a second copy from its journal credit. Existing authority and organization filters are unchanged.

For v2 fee effects, `timeBasis=booking` selects the actual ledger write date, `service` selects the completed service date (activation uses service start; correction/dispute uses action date), and `payment` groups recognition/cashless void by original order payment date while refund/dispute cash uses action booking date. No filter returns the cumulative position; a filtered deferred/receivable figure is a signed movement, not the period-end balance. V1 bill/collection/receivable cohorts retain their historical posted/issued-date semantics. Refund cash uses allocation booking or proven ledger booking for every basis; noncash discounts use line creation date. Usage service reads use settlement entry booking dates. These source clocks are explicit and must not be interpreted as a statutory financial statement.

New v2 `planStatement` evidence artifacts include `accounting.policyVersion`, the immutable `period`, listed `effects`, `capturedAt` and a precise scope statement. Each nonnull effect entry must be included in the bundle and verify against its head. Re-download/re-mint retains existing bytes: later recognition/refunds are not silently added. The export proves captured source integrity and inclusion, not actual delivery or future completeness.

### Application namespaces and customer lifecycle (#846)

The owner creates a stable namespace with `POST /api/v1/business/applications`
(`idempotencyKey`, lowercase `key`, `name`, `customerLimit`, `reason`) and lists
namespaces with the same path's GET. Limits count every retained account,
including archived identities. No application or customer identity can move.

An owner-issued integration credential may select `applicationId` with empty
`customerIds`. Its explicit scopes reach only customers provisioned in that
application, including after credential replacement. Existing credentials without
an application keep their explicit customer grants and credential-local identity.
`customersLifecycleWrite` is available only with an application and grants archive,
not reactivation, identity linking, staff access or balance changes.

Application credentials use `POST /api/v1/integration/customers`,
`GET /api/v1/integration/customers?externalId=...` and
`POST /api/v1/integration/customers/{organization_id}/archive`. Responses contain
`applicationId`, the application-local `externalId`, and `customer`; the latter
retains its original business external identity and ledger organization ID. The
same caller ID in two applications creates two independent accounts. No email
match or explicit allowlist entry adopts an existing human account.

Provision and archive keys share the application operation namespace. Exact
normalized retries return the original receipt, including after rotation or later
archival. Changed reuse conflicts; another key cannot provision an existing ID.
Read the customer to see its current state. Archive serializes with admission,
retains historical money/evidence and permits already admitted holds to settle.
Legacy business provisioning also pins normalized request keys; matching an
external ID alone is no longer sufficient to claim a standing account.

Application metering keys bind application/customer/operation rather than the
replaceable credential. A replacement credential with current `meteringWrite`
may recover the same hold/outcome without charging twice. Original credential
attribution, price and terms remain pinned. Changed usage or an unrelated
application cannot take over the operation. These APIs do not yet supply scoped
key management, checkout, human consent linking or the full external application
journey tracked by #747.

### Independent financial comparison (#848)

Reconciliation run `periodStart` and `periodEnd` are inclusive calendar dates serialized as `YYYY-MM-DD`, including manual creation, history lists and detail responses. They are not year/ordinal tuples. Retained comparison snapshots and original financial evidence are unchanged.

`POST /api/v1/business/reconciliation-runs` accepts `periodStart`, `periodEnd`,
`idempotencyKey` and optional `timeBasis` (`booking`, `service`, `payment`; default
`booking`). It requires a current personal `FundsWrite` session. One successful
request binds its original dates, clock and immutable snapshot; an identical
retry returns that original result, even after another run. A reused key with
different terms returns `409`. An already active identical run returns
`429 POOL_BUSY` for a later retry. The interval may contain at most 366 days.

Run responses expose `coverage`, `timeBasis` and `latestSnapshotId`. Only the six
fields listed in `coverage.comparedFields` have independent actual totals:
`billedMinor`, `earnedMinor`, `deferredMinor`, `collectedMinor`, `receivableMinor`
and `refundMinor`. Usage, rating, entitlement, funding, noncash discounts,
provider costs and complete business accounting remain outside this comparison.
A missing or unsupported source makes coverage incomplete; amount disagreements
produce source/fact exceptions even when the document's conservation identity
still balances. Neither a matched result nor complete source coverage proves
actual service delivery or independently authenticated provider truth.

Legacy v1 totals are captured net journal positions selected by original posting
date, including later source corrections. This coherent journal cohort is not
the financial-facts dashboard's legacy receivable filter by statement issue
date. V2 totals are signed effect movements under the selected clock, with
booking derived from verified official entries and service/payment attribution
bound to original sources. Refund cash follows the retained original cash
cohort; corrections without Cash do not count twice. Payable and suspense stay
in the detailed source evidence rather than income or receivables.

`GET /api/v1/business/reconciliation-runs/{runId}/snapshots` lists immutable
attempts newest first by `(capturedAt, snapshotId)`; `after` names the last
snapshot and `limit` defaults to 50, capped at 200. List entries omit `evidence`.
`GET /api/v1/business/reconciliation-runs/{runId}/snapshots/{snapshotId}` returns
the exact captured sources, verified postings and differences under current
personal `ReportsRead` authority. An ID from another run returns `404`.
Pre-upgrade projection-only runs have retained `legacy` snapshots with coverage
version 0 and never become independent evidence retroactively. Current exception
resolution/waiver status is a separate work list; it cannot rewrite a captured
attempt. Bounds or storage failures leave the previous run/snapshot intact.

Application-bound `customerKeysWrite` enables
`GET/POST /integration/customers/{organization_id}/keys` and
`POST /integration/customers/{organization_id}/keys/{key_id}/revoke`. The exact
fields and bounds are in OpenAPI. This scope cannot be issued to a legacy grant;
only an application's own provisioned customers and issued keys are eligible.
Creation's `secret` is non-null exactly once. Subsequent exact operation replays,
including through a rotated management credential, return the original metadata
with `secret: null`; list reads current metadata. Current authority is checked even
on replay, and changed operation terms conflict across create/revoke/customer
operations sharing the same application namespace. The API does not recover lost
plaintext: explicitly revoke and intentionally create a replacement.
Gateway settlement v15 and guardrail configuration (#875) use the
[bounded evidence contract](gateway-evidence-budget.md). New required-policy
lists and bindings permit at most eight distinct names; their effective union
is also checked before execution. A complete reason larger than 256 encoded
UTF-8 JSON bytes is represented exclusively by `reasonCommitment` with SHA-256
and UTF-8 length. Prior settlement versions remain readable.
The ledger-backed `BillingRecord` carries `failureReason` for complete inline
text or `failureReasonCommitment` for a long reason. These fields are exclusive;
the read projection does not claim separately retained full text was verified.
Keyword configuration permits 1–64 patterns, each regex at most 4,096 UTF-8 bytes.
Moderation bodies are limited to 65,536 bytes within the existing deadline.

Historical non-plan chargeback compensation (#842) adds nullable
`OrderDispute.cashCorrection` and finance audit action `nonplan_chargeback.recover`.
The local owner/finance preview/apply has no HTTP mutation endpoint. Original
wrong receipt B plus actual loss L produces appended Cash credit B + L, with
separate fields for both components, original Wallet debit, review suspense,
preserved credit benefit, exact entry/receipt identities, hashes and source
revision. Null means no retained correction, not historical correctness; the
read projection does not independently reverify the evidence. Original
terminal/audit/debt history remains unchanged. A valid corrected `lost` replay
retains its standing result without another money leg. An exact earlier resolution
request also returns current standing; a new request for another outcome conflicts.


### Guardrail refund and request fees (#899)

A whole-response guardrail refusal, including a cache hit, charges zero customer usage and zero fixed request fee and releases the entire wallet reservation and subscription draw. Streamed content already delivered remains billed; a successful zero-token answer still owes its configured request fee. A customer refund does not imply zero provider cost: retained provider usage remains on the actual attempt and prices operator cost; absent provider counts remain untracked. A refused cache hit makes no new provider attempt. Exact request replay retains the same receipt and does not repeat a charge or refund.
## Immediate customer receipt readiness

`GET /api/v1/org/readiness/receipt` requires a personal customer session and
applies the same member/key record scope as receipt proofs. It checks at most 32
newest eligible accounting rows, independently of operational request logs.
`ready` carries an exact request, entry, canonical hash, model, total priced usage and the separate wallet charge;
zero-cost and fully quota-funded success qualify. `missing` means no eligible row was found;
`incomplete` means available evidence did not establish readiness within the
bound. Failed/refused calls and whole-response blocks do not qualify. Reads
validate attribution, entry identity, charge arithmetic, posted revenue and log
inclusion without writing accounting data or creating a ledger.

`/verify?entry=<uuid>` prefills the existing verification form from a current
personal session's scoped receipt. Foreign/invisible entries return 404,
missing sessions return 401 and explicit machine headers return 403 rather than
inheriting a cookie. Malformed entry IDs return 400. Plain `/verify` and its
public sample remain available without authentication. The prefilled hash comes
from this server and is labeled accordingly; retaining it and independent head
trust are separate from internal inclusion/arithmetic checks.
### Application entitlement and customer sale reads (#898)

An owner can explicitly grant application-only `entitlementsRead`. Existing
credentials gain no permission. `GET /api/v1/integration/customers/{organization_id}/entitlement`
returns recorded customer restrictions, sold-plan lifecycle, quota window/counters
and shared plan debt. Recorded remainder includes pending draws and is never a
promise of admission after cancellation, expiry or a restriction.

`GET /api/v1/integration/customers/{organization_id}/models` filters configured
public models by current customer, alias-target and optional application-issued
key/group permissions before pagination (`limit` 1–50, default 20; `after` is the
last public name). `keyId` must be live and belong to this exact application and
customer. Effective published customer sale terms use override, assigned-book,
then default precedence. `saleTerms: null` means unavailable, without substituting
wholesale channel prices. Provider names, destinations, secrets and costs stay
private. Every page chooses its own database snapshot.

These reads never reserve, roll quota, refresh dunning, publish a sale digest,
write a bill or contact a provider. They show current recorded/configured facts;
actual request admission remains authoritative. Personal sessions and consumption
keys cannot use these application management routes.

### Application account consent (#920)

Application-only `customerLinkWrite` enables
`POST /api/v1/integration/customers/{organizationId}/link-invitations` with
`idempotencyKey`, `intendedEmail` and `reason`; an exact create replay returns
metadata with `token: null`. The first response includes a 256-bit random secret,
stored only as its SHA-256. Deliver it as `/app/application-link#<token>` through
the application's existing authenticated flow. Do not put it in query strings,
logs, analytics or email from this API. A lost secret requires explicit revocation
and a new operation key; rotation does not recover it.

`POST .../link-invitations/{invitationId}/revoke` uses the current application
credential and an exact keyed reason. Revocation cannot undo recorded consent.
`POST /api/v1/application-links/preview` and `/accept` take `{token}` in a JSON
body and require a personal session, matching verified email and current second
factor. They accept either session context and never switch it. Machine bearers
are refused even alongside a cookie. Foreign customers return 404, missing scope
or recipient authority 403, revoked/expired credentials 401, and expired/revoked
invitations or already claimed accounts 409. A completed consent retry returns
its original terminal metadata without granting membership again. Invitations
expire after seven days; old grants do not gain the new scope automatically.

### Application frozen evidence routes (#924)

`GET /api/v1/integration/customers/{organizationId}/evidence-bundles` requires
application-only `evidenceRead`; `after` is the prior bundle UUID and `limit` is
1–50 (default 20). It returns `{data:{applicationId,organizationId,bundles,nextCursor}}`
in ascending UUID order. Descriptors include all retained files; listing is not
an integrity or downloadability verdict.

`GET .../evidence-bundles/{bundleId}` returns original version-2 JSON directly,
without a `data` envelope, with `Content-Type: application/json`, an attachment
filename, `Cache-Control: private, no-store` and `X-Content-Type-Options: nosniff`.
Missing/foreign account or bundle is 404, missing scope 403, invalid current
credential 401. Unsafe, inconsistent, malformed or over-16-MiB retention is 409:
obtain operator recovery or a smaller scoped export, never retry as a different
customer. Unknown extra top-level fields, duplicate JSON keys, uncommitted
artifact additions and upstream-cost figures fail closed. Consumers retain the
file unchanged and verify it with their own held trust inputs and documented
proof scope. A server-side consistency check does not establish independent trust.
### Exact statement repayment (#777)

`GET /api/v1/org/statements/{statementId}/payment-quote` requires a personal
customer session. Optional positive `amountMinor` selects credit-minor debt;
omitting it selects all unreserved debt. Foreign statements are 404. The
response includes outstanding/reserved/available amounts, pending order IDs,
exact journal/plan slices, pinned collection terms and a fingerprint. Review
reasons preserve the displayed debt but omit payable terms. Old duplicated
usage must be corrected before collection even when its recorded AR balances.

Create `/api/v1/orders` with `purpose: statementRepayment`, `purposeRef` equal
to that statement, `statementAmountMinor`, `expectedStatementQuote` and the
exact fiat terms. `amountMinor`/`rate` remain invalid creation fields. Changed
quotes/fiat or reused keys with different terms refuse before provider I/O;
exact-key retries keep their original price and checkout after collection
changes. The reservation and binding commit before provider execution. Browser
return/cancellation and elapsed time never establish payment or release debt.

The immutable `statementPayment` field exposes original source allocations on
customer and business order reads. Fulfillment `purposePayload` names
`repaidMinor`, `payableMinor`, `requiresReview` and source-linked entries.
Refunds use existing customer request, personal finance approval and verified
merchant-event paths; dispute resolution commits decision, cash and audit in
one transaction. Canonical reconciliation evidence retains exact reversal
entries and void-source allocations. The existing `invoiceRepayment` purpose
remains Cash/CreditLine and its reference is not a plan-statement allocation.

Finance uses `GET /api/v1/business/orders/{orderId}/statement-payment-evidence`
under a current personal `ReportsRead` capability to read original payment and
return allocations, canonical entry IDs/hashes and Cash/AR/payable/suspense.
The read opens only an existing ledger and refuses damaged retained effects.
Unknown, foreign-business or other-purpose orders are 404. It creates no payment,
refund, finance decision or source repair.

The existing journal void action now retains source-specific corrections for
mixed documents (#927). A statement release-review `void` still refuses a mixed
document and asks finance to select its original journal; it cannot silently
interpret a whole-document cancellation as a partial source correction. Each reversing line's
`sourceRef` names its original statement line. New frozen statement artifacts may
include `sourceVoids` with the original/reversal IDs, journal ID and canonical
void entry/hash. These are committed source assertions; independent runtime
reconciliation checks their financial ownership. The generic offline verifier
checks manifest inclusion/integrity, not this additional business interpretation.

### Application checkout (#930)

All routes below require application-only `checkoutWrite` for the exact namespace
and customer. They reject owner cookies, consumption keys and legacy grants.

- `POST /integration/customers/{organizationId}/checkout-quote`: select
  `walletTopup` with positive `displayAmountMinor`, or `bundle` with a published
  `bundleVersionId`; include the selected `rail`. The quote returns exact
  server-derived credits, promo, fiat, collection revision, merchant choice,
  public name and test/live mode. It is read-only, not a payment reservation.
- `POST /integration/customers/{organizationId}/orders`: submit `idempotencyKey`,
  the unchanged `quote`, `returnUrl` and `cancelUrl`. Absolute HTTPS is required;
  literal loopback HTTP/localhost supports local development. Userinfo, fragments,
  control characters, whitespace and backslash normalization are rejected.
- `GET /integration/customers/{organizationId}/orders`: UUID-ascending page of
  application-created orders (`after`, `limit` 1–50, default 20).
- `GET /integration/customers/{organizationId}/orders/{orderId}`: read that
  application-created order without provider I/O.
- `POST /integration/customers/{organizationId}/orders/{orderId}/resume`: recover
  original provider checkout using retained destinations and merchant version.

These paths share the `/api/v1` prefix and ordinary data/error envelopes. Missing
namespace/order is 404, missing scope 403, expired/revoked identity 401, stale
quote/changed retry/archive 409 and unknown provider outcome 503. A 503 may leave a
durable created order: retry the same request or list and resume that exact order.
Return `clientSecret` only to the corresponding application user; never treat
checkout handles or browser return as payment confirmation. Archived accounts
retain reads, while create/resume refuse. Assigned plan payment is described
below; named statement debt remains separate. Personal checkout keeps its own
customer-session authority.

### Assigned application plan payment (#931)

The application checkout selection additionally accepts `purpose: planPeriod`
and `customerPlanId`, with both `displayAmountMinor` and `bundleVersionId` absent.
`planPayment` in the returned quote contains the exact sold fee and period terms;
`purchasedMinor` and `promoMinor` are zero. Submit that complete quote unchanged
through the existing application orders route. A foreign assignment is 404; a
changed/nonpayable quote or already-standing period intent is 409. No standing
portal order identifier is exposed; application order history returns only its
own recoverable intents. This does not add named statement debt repayment.

### Application evidence publication (#934)

`POST /api/v1/integration/customers/{organizationId}/evidence-publications` uses
explicit application-only `evidenceWrite`. Send `idempotencyKey`, `kind: period`
with `from`/`to` (YYYY-MM-DD, up to 31 ended UTC days), or `kind: statement` with
`statementId`. Mixed selections, open days and oversized spans refuse. Success
returns `{requestId, status: "published", bundle}`; the descriptor has the normal
evidence summary shape. `evidenceRead` separately downloads original bytes.

Authorization is durable before publication. A failed response may leave an
accepted request or completed marker; retry the identical operation instead of
creating another scope. Changed payload/customer conflicts; missing scope is 403,
foreign customer/statement 404, revoked/expired identity 401, archived initiation
or private/oversized evidence 409. Accepted work may finish after revocation;
response authority is rechecked. No share token, supplier cost or money mutation.
### Scoped application named statement payments (#935)

Application `checkoutWrite` also accepts `statementRepayment` at
`POST /integration/customers/{organizationId}/checkout-quote`. Supply exactly
`statementId` and optionally `statementAmountMinor` (positive credit minor units);
omit the amount for all verified unreserved debt. Wallet fiat, bundle and plan
selection fields must be absent. The response's `statementPayment` pins original
receivable allocations, exact debt and collection terms. `purchasedMinor` and
`promoMinor` remain zero. A fiat ceiling does not change the selected debt.

Submit that complete quote through the existing application order route, retaining
the operation key and return URLs. The ordinary order transaction rechecks current
authority and debt sources under existing ownership/append locks before reserving.
Unknown outcomes retain their reservations and original provider identity; only
verified callbacks release or fulfill them. Portal/foreign application pending
order IDs are never exposed by this quote. Unavailable/review-required debt is a
409, foreign statements are 404, and unauthorized scope is 403.

### Application statement history (#939)

Application-only `billingRead` now reads `/integration/customers/{organizationId}/statements`
and `/{statementId}`. Both require current exact namespace/customer authority;
ordinary customer-list credentials without an application do not gain this surface.
Archived customers retain authorized history. No additional scope or implicit
checkout/evidence authority is introduced.

The list is newest-first with `limit` 1–50 (default 20) and an `after` identifier
that must itself be a non-open statement belonging to this customer. Missing,
open and foreign cursors/details return 404. Detail returns the existing complete
customer-visible header and all ordered lines, including original source references.
Statement money is integer credit minor units, not the checkout's rounded fiat amount.

Authorization and data use one repeatable snapshot, retaining the existing
credential revocation lock. PostgreSQL measures variable source data before
transfer: a list/detail is bounded to 4 MiB and detail to 1,000 lines. Oversize
returns 409 for operator recovery; it never truncates a successful document.
These live reads do not publish evidence or certify its proofs, reserve payment,
open wallets or alter any financial fact. Export independent evidence separately.
## Immediate customer receipt readiness

`GET /api/v1/org/readiness/receipt` requires a personal customer session and
applies the same member/key record scope as receipt proofs. It checks at most 32
newest eligible accounting rows, independently of operational request logs.
`ready` carries an exact request, entry, canonical hash, model, total priced usage and the separate wallet charge;
zero-cost and fully quota-funded success qualify. `missing` means no eligible row was found;
`incomplete` means available evidence did not establish readiness within the
bound. Failed/refused calls and whole-response blocks do not qualify. Reads
validate attribution, entry identity, charge arithmetic, posted revenue and log
inclusion without writing accounting data or creating a ledger.

`/verify?entry=<uuid>` prefills the existing verification form from a current
personal session's scoped receipt. Foreign/invisible entries return 404,
missing sessions return 401 and explicit machine headers return 403 rather than
inheriting a cookie. Malformed entry IDs return 400. Plain `/verify` and its
public sample remain available without authentication. The prefilled hash comes
from this server and is labeled accordingly; retaining it and independent head
trust are separate from internal inclusion/arithmetic checks.

## Personal customer serving readiness

`GET /api/v1/org/readiness/serving` accepts only a current personal customer
session. Owners/admins see their account's live keys; members see their own.
It checks at most eight live keys and sixteen permitted chat models per key,
with customer, group and alias-target restrictions applied before paging.
`scanLimited` explicitly preserves the possibility of another eligible choice.
The selected sample is the first funded choice, otherwise the first priced
choice; `reason` explains missing, restricted, transition or funding states.

The sample is a single user message `Hello`, with `max_tokens: 32`, no tools,
attachments, service tier or experiment. The gateway's conservative text-bound
pricing includes conditional rates and flat fees. It uses exact effective
customer sale terms, never supplier prices; the reservation is undiscounted,
as in gateway admission, with the sale discount retained for final settlement.
The response separates the total bound, recorded quota and wallet remainder.
A missing ledger reports `ledgerUninitialized`; no read bootstraps accounts.

Sale selection, authority, quota and funding share one read-only repeatable-read
transaction. Registry identities are opened beforehand without writes, so a
one-connection database pool works. No key secret, provider destination or
provider cost is returned. This is not a reservation or an execution guarantee:
rate windows, policies and provider availability remain actual-call checks.
### Application lifecycle notifications (#937)

Owner-only `IntegrationsManage` configures immutable destinations at
`/business/applications/{applicationId}/notification-endpoints`: GET lists at
most twenty retained endpoints; POST takes exact operation key, reason, URL,
destination policy and distinct event names. At most two can be enabled. The
first create response returns the signing secret once; exact replay returns null.
A POST to `/{endpointId}/disable` stops new enqueue and unclaimed delivery.
A claimed attempt can still finish. Address/key rotation creates a replacement.

GET `/{endpointId}/deliveries?after=...&limit=...` returns at most fifty statuses.
POST `/{endpointId}/deliveries/{deliveryId}/retry` starts a new ten-attempt cycle
only for a failed delivery to an enabled endpoint; original ID and bytes remain.
Both writes use an audited reason and stable operation key; changed reuse conflicts.
The application-only `notificationsRead` scope reads its own endpoint metadata at
`/integration/notification-endpoints` and delivery pages at
`/integration/notification-endpoints/{endpointId}/deliveries`. It grants no
destination changes, signing secret recovery, foreign endpoints or implied reads.

Signals are `customer.changed`, `key.changed`, `entitlement.changed`,
`order.changed`, `statement.changed`, `evidence.published`, `balance.changed`.
The exact signed envelope contains version 1, delivery `id`, `type`, applicationId,
organizationId, entityId, entityKind, action `readCurrentState` and createdAt.
It contains no financial state or credentials. Headers use the existing
`x-oxsum-event`, `x-oxsum-delivery` and `x-oxsum-signature: t=...,v1=...` format:
HMAC-SHA256 of ASCII timestamp, dot, original UTF-8 body. Verify within a bounded
time window before parsing; persist delivery IDs and reject changed byte replay.
The timestamp is not a state version. Reconcile using separately granted current
reads, and independently verify any downloaded evidence.

## Business monthly credit terms

`GET /api/v1/business/customers/{organizationId}/credit-terms` requires a personal Business session with `reportsRead`. It returns `organizationId`, `creditLimitMinor`, `creditUsedMinor` (drawn plus reserved), `paymentTermsDays` and nullable `dunningGraceDays`. Reading an unopened wallet does not create a ledger.

`PUT` on the same path requires `commercialWrite` (owner/finance) and `{idempotencyKey, creditLimitMinor, paymentTermsDays, dunningGraceDays, reason}`. Every field is required; explicit `null` disables monthly dunning. Credit is an integer from 0 through 9007199254740991; terms/grace are whole days from 0 through 3650. The total credit facility, terms, actor audit and immutable mutation receipt commit atomically. A limit below drawn/reserved usage is `400 VALIDATION_ERROR`; a reused key with different content or a new decision for an archived customer is `409 CONFLICT`. An exact successful historical request returns the original mutation ID without reapplying its terms, including a no-op credit change. Foreign customer IDs are `404`.

These operations reject explicit machine credentials even alongside an owner cookie; consumption/application/deployment credentials are not substitutes for staff authority. The monthly facility remains separate from sold-plan credit, and changing it does not rewrite issued documents or clear manual/archive restrictions. See the OpenAPI schemas `BusinessCustomerCreditTerms` and `BusinessCustomerCreditTermsRequest`.


## Business monthly statement inspection

`GET /api/v1/business/customers/{organizationId}/monthly-statements` and
`GET …/monthly-statements/{statementId}` require a personal Business session
with `reportsRead` (#1007). Explicit machine credentials are denied even alongside
an owner cookie. Unknown customer IDs and cross-customer statement pairs return
404. These are monthly wallet-credit documents, separate from the existing
sold-plan `/business/statements` endpoints.

The list accepts `limit` (1–100, default 20), exclusive UUID `after`, and optional
fully ended UTC `period` (`YYYY-MM`). It returns `{statements, nextCursor}` in
ascending immutable-ID order. Detail returns `{statement, lines}` and refuses
more than 1,000 lines or 2 MiB of expanded serialized line data rather than
truncating. The statement carries original terms, money and stored proof-window
identifiers. Display-only `paymentStatus` becomes `overdue` when a finalized
pending statement has unpaid credit after its UTC due date; stored timestamps
and rows remain unchanged. Payment figures are the recorded projection, not a
new ledger reconciliation. The read neither initializes a wallet nor mints a
proof or authorizes generation, finalization or repayment.


## Reviewed monthly staff actions (#1011)

`POST /api/v1/business/customers/{organizationId}/monthly-statements/actions`
requires a personal session in Business context with `fundsWrite`. Owner and
finance can generate one closed UTC month or finalize one customer-bound draft;
customer sessions and explicit machine credentials refuse. Both intents require
an exact `idempotencyKey` and `reason`. Financial dates are server-owned.

Generation returns `{id, statementId}`; an empty unbilled month returns null for
`statementId`. The immutable outcome remains the same even if usage arrives or
the statement later advances. Detail adds nullable `reviewDigest`: drafts bind
the displayed usage totals, credit draw, itemization, terms, original identity
and current UTC review day; issued documents return null. Finalize supplies that
digest. Changed review content refuses with 409 and rolls back all effects; the
client regenerates, rereads and reviews with a new intent. This is a review
binding, not independent source attestation or proof of usage completeness.

Authority and customer ownership are rechecked inside the Business-locked write.
Archived customers refuse new writes while exact historical receipts still need
current authority. Anchor, document, lines, event, mutation receipt and actor
audit commit together. A retry after a lost response returns the original receipt
without rerunning finalization. Changed action/target/reason/digest under an old
key conflicts. Existing monthly customer proof/PDF routes remain unchanged.



### Staff monthly credit repayments (#1015)

`POST /api/v1/business/customers/{organizationId}/monthly-statements/{statementId}/payments`
requires a personal Business owner/finance session with `fundsWrite`. Explicit
machine credentials refuse even beside a valid cookie. Send exact `amountMinor`,
`reference`, `reason` and `idempotencyKey`; financial dates are server-owned.
Amounts are positive safe integers in credit minor units. References preserve
exact case/spacing, have 1–200 characters and no surrounding whitespace/control
characters. They must identify already verified incoming money.

The selected owned document must be issued. The credit cap covers all unpaid
draws through its month, including earlier unbilled draws, and no later-month
debt. Only Cash to CreditLine is posted. A reference is globally unique across
manual receipts, manual payment orders and monthly repayments, including other
customers and staff actors. Reference conflicts return 409 without a payment, reference claim or
actor mutation. All financial, event and actor-audit effects commit together.

The response is immutable `{id, organizationId, statementId, amountMinor,
reference, entryId, contentHash}`. Exact successful retries return those original
values after full payment or archival, under current authority. A new archived
write refuses. Changed intent conflicts; missing/foreign document pairs return
404 before wallet initialization. No provider call or plan allocation occurs.
### Invoice correction and recovery (#1027)

Personal Business FundsWrite sessions use
`POST /api/v1/business/journals/{journalId}/correction-preview` with original
line IDs and exact nonnegative minor amounts. The response binds a source
revision, unchanged service terms, all line changes, original payable and
successor debt/recognition separately. Preview creates no reservation.

`POST /api/v1/business/journals/{journalId}/correct` requires those selections,
`sourceRevision`, `reason` and `idempotencyKey`. Stale sources or altered replay
conflict. Exact retries return the original correction and successor IDs after
later activity; current authority still applies. A client with an unknown
response retains the exact request/key for recovery. Optional
`successorJournalId`/`successorStatementId` detail fields navigate replacements.
