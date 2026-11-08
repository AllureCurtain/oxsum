# Unsupported and refused behaviors

The calls this API turns away, and the exact refusal each one gets. Every row is
a behavior a caller can test against — the error code is the contract, not an
implementation detail that might soften over time. The counterpart that says
what *is* wired is [integrations.md](integrations.md); the error envelope
itself (`{"error": {"code", "message"}}`) is `docs/api.md`.

## Schema rejections — 400 `VALIDATION_ERROR`

These fail before any business rule runs: `additionalProperties: false` on the
request schema means an unrecognized field is a rejection, never ignored.

| Refused call | Refusal | Why |
| --- | --- | --- |
| `POST /api/v1/orders` with `amountMinor` or `rate` in the body | `400` — `OrderCreateRequest` carries only `idempotencyKey`, `rail`, `purpose`, `displayAmountMinor`, `displayCurrency`, `purposeRef`, `promoMinor`, `returnUrl`, `cancelUrl` | the ledger amount is the server's conversion of `displayAmountMinor` at the rate it owns; a client-quoted figure could never be a fact (`crates/server/openapi.yaml` `OrderCreateRequest`) |
| `POST /api/v1/business/integration-credentials` with an unknown scope, wildcard customer, nonempty application `customerIds`, or legacy lifecycle scope | `400` — scopes are `customersRead`, `meteringWrite`, `customersProvision`, `billingRead`, application-only `customersLifecycleWrite`; legacy grants need 1–1000 UUIDs except provision-only may start empty | application grants reach only their provisioned accounts; neither grant admits a wildcard (`openapi.yaml` `IntegrationCredentialRequest`) |
| `POST /api/v1/orders` with a `purpose` of `planPeriod`/`invoiceRepayment` and no `purposeRef` | `400` | the arm needs the receivable or plan it settles (`crates/core/src/payment_orders.rs`) |
| `stream: true` on `POST /v1/embeddings` or `/v1/rerank` | `400` — "this surface does not stream" | input-only surfaces price the request, not a stream (`crates/server/src/gateway/request.rs`) |
| A media content part — `input_image`/`input_file`/`input_audio` on `/v1/responses`, an `image`/`document`/`audio`/`video` block on `/v1/messages`, `inlineData`/`fileData` on a Gemini call | `400` — the part "has no computable input bound, so it cannot be frozen" | a hold must name a figure before upstream is contacted; an unbounded input cannot be frozen honestly |
| `POST /api/v1/admin/organizations/{id}/manual-receipts` with `amountMinor` ≤ 0 or a reused `reference` | `400` / `409` | receipt terms are validated; `reference` is deployment-wide unique |

## Authority refusals — 401/403/404 as designed

Crossing an authority boundary is refused the way the boundary is defined —
often *not found* rather than *forbidden*, so a probe learns nothing about
what exists.

| Refused call | Refusal | Why |
| --- | --- | --- |
| `POST /api/v1/topups` (the retired customer manual top-up) | always `403` | customers may initiate a checkout or redeem a code, never assert money arrived; deployment receipts go through `POST /api/v1/admin/organizations/{id}/manual-receipts` (`openapi.yaml:3688`) |
| `POST /api/v1/holds` / `POST /api/v1/settlements` as a customer | `403` | customers freeze through the gateway, never directly; the metering surface is for `meteringWrite` service credentials |
| A metering call with an `oxs-int-` credential lacking `meteringWrite`, or for a customer outside its explicit grant or application | `403` on scope; `404` on a foreign customer | scope and the explicit grant/application mapping are rechecked in the authorization transaction; a foreign customer is *not found* so customer ownership cannot be probed |
| An integration credential on `GET /api/v1/operations`, `/api/v1/bills`, `/api/v1/balance` — the customer's own records | `401`/`403` | `customersRead` grants the *business* customer list/detail only; the customer's consumption records belong to the customer |
| A customer's own `oxs-` key reading rows another credential booked — e.g. the `metering` operation a service's `oxs-int-` wrote | not an error: the rows are simply absent from `GET /api/v1/operations`/`/api/v1/bills` | customer-facing reads are scoped to the key that booked them (`CustomerRecordScope`); a member session or `GET /api/v1/admin/operations?organizationId=…` is the whole-org view |
| `POST /api/v1/business/integration-credentials` as a member key, staff short of `integrationsManage`, or a session without current TOTP | `403` | minting is owner authority + second factor; a consumption key can never mint credentials |
| `POST /api/v1/business/refunds/{id}/resolve` or `/disputes/{id}/resolve` as a customer, or staff lacking `RefundsWrite` | `403` | refund approval and dispute resolution are staff-only; the customer's side is `POST /api/v1/orders/{id}/refund-request` |
| `GET /api/v1/business/orders`, `/refunds`, `/disputes`, `/statements`, `/journals` as a customer | `403` | the business read surfaces need staff `ReportsRead`; customer membership never implies business authority |
| `POST /api/v1/auth/register` when the business policy is invite-only | `403` `FORBIDDEN` | self-registration is an explicit owner choice, not a default; `OXSUM_SIGNUP` cannot override it |
| An expired, revoked, exhausted or unknown evidence-share token on `GET /api/v1/evidence/{token}` | `410` — indistinguishable | a guessed token learns nothing about which shares exist |
| A signed merchant webhook on an unknown merchant/version coordinate | `404` | the path selects the signing secret; a guessed coordinate names nothing |
| A mis-signed merchant webhook | `401` | signature verification runs before any parsing |

## Replay and dedup refusals — 409 `CONFLICT`

The idempotency contract: same key with same terms replays; same key with
changed terms conflicts.

| Refused call | Refusal | Why |
| --- | --- | --- |
| `POST /api/v1/webhooks/merchants/{id}/versions/{vid}` with a provider `event_id` already received under different bytes | `409` — "merchant event ID was reused with different content" | the payload digest pins what the id first claimed; a mutation cannot rewrite the intake record (`crates/core/src/merchant_events.rs`) |
| Any write endpoint's `idempotencyKey` replayed with changed terms (order create, metering settle, integration-credential mint, manual receipt) | `409` | a key names one operation; identical terms return the original result, changed ones conflict — the original bytes win |
| A settled metering hold settled again, or a hold that is not outstanding | `409` / `404` | the settlement is derived from the hold's key and lands at most once |

## Not routed — 404

These paths do not exist in the router at all; a call falls through to the
page fallback and answers 404.

| Not served | Where it stands |
| --- | --- |
| `/v1/images`, `/v1/audio/*`, `/v1/fine_tuning`, `/v1/batches`, `/v1/moderations`, `/v1/assistants`, threads, `/v1/vector_stores` | no surface, no adapter — the `event` price mode can *meter* tool/image/audio/video token dimensions a service reports through `/api/v1/metering`, but the gateway forwards no such upstream request |
| A batch embeddings API | `/v1/embeddings` is the synchronous single call; a token-array `input` is its batch spelling, not a job endpoint |
| `/v1/files`, `/v1/uploads`, fine-tuning job management | not wired |
| Long-running media generation as a wire-shaped endpoint | reachable only as the generic task relay `POST /v1/tasks` on a `protocol: tasks` channel — submit then poll, no provider-specific shape |
| Provider-account import OAuth | deferred — issue #506 is the named gap, documented so no reader assumes coverage |

## Behavior boundaries — refusals inside a supported surface

| Refused condition | Refusal | Why |
| --- | --- | --- |
| Event usage naming a dimension the resolved `event` price cannot bill | `400`, never partial-billed | the service declares the usage; an unpriceable dimension is fixed by the caller, not silently zeroed |
| A metering call for an insolvent customer | `402` `INSUFFICIENT_FUNDS` | a one-shot settlement cannot spend what was never there |
| A metering call for a suspended customer | `403` | suspension halts billing authority the same as consumption |
| A member key over its `spendLimitMinor` | `429` `KEY_LIMIT_EXCEEDED` on `/api/v1`, `insufficient_quota` on the gateway | the cap is enforced at admission — before upstream spend, not after |
| Semantic-cache reuse across different sale terms | the `sale_digest` partitions the cache key — a hit under different terms is a miss, never a cross-serve | an answer priced under other terms cannot be served as this customer's own (`x-consumption-cache`, `openapi.yaml:5`) |
| A payment rail's intent POST with the rail unconfigured | `503`; its inbound webhook `404` | a half-configured rail refuses to boot; an absent one names no handler |
| A signed price/mandate/expired credential replayed where not valid | 401/403 per surface | capabilities are verified per call, not cached beyond their terms |
