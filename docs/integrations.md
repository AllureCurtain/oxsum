# Supported integrations

The exact protocol, provider and parameter surface this build serves — and the one it
refuses. The contract is `crates/server/openapi.yaml`: every row here names a routed
path, and each Test coverage column names the local suite that exercises it. A capability an upstream
provider happens to offer is not a row unless this server wires it; [unsupported.md](unsupported.md)
is the refusal catalog — what the API turns away and the error it answers.

These suites use controlled local fixtures unless a retained evidence record
explicitly names an external provider, model, protocol and executed artifact.
They establish software behavior, not universal provider or PSP certification.
Current payment tiers and credential limitations are in [rails.md](rails.md);
T31 evidence and pending external gates are in
[the release checklist](roadmap/self-hosted-v1/RELEASE-CHECKLIST.md). A historical
non-streaming DeepSeek call does not certify streaming, other protocols/models,
or a newly changed build. HEAD reachability is not an inference result.


## LLM gateway

Every surface runs the same pipeline — freeze an upper bound before upstream is
contacted, settle against actual usage after it answers (`crates/server/src/gateway/mod.rs`).
Each surface is protocol-native: a channel declares one `protocol` and serves only the
surfaces that speak it; a model an `anthropic` channel serves is never translated onto
the OpenAI surface (`Surface::protocol`, `crates/server/src/gateway/request.rs:38`).

| Surface | Channel protocol | Price mode | Streams | Test coverage |
| --- | --- | --- | --- | --- |
| `POST /v1/chat/completions` — OpenAI chat body | `openai` | `chat` | SSE via `"stream": true` | `crates/server/tests/gateway.rs`, `apis.rs` |
| `POST /v1/responses` — OpenAI Responses wire (issue #495) | `openai` (upstream path `responses`) | `chat` | SSE via `"stream": true` | `crates/server/tests/gateway.rs`, `serving.rs` |
| `POST /v1/messages` — Anthropic Messages body | `anthropic` | `chat` | SSE via `"stream": true` | `crates/server/tests/serving.rs` |
| `POST /v1/embeddings` — OpenAI input-only shape (issue #170) | `openai` | `embeddings` | no — `"stream": true` is a parameter refusal | `crates/server/tests/gateway.rs` |
| `POST /v1/rerank` — Jina/Cohere shape (issue #170) | `openai` | `rerank` | no — same refusal | `crates/server/tests/gateway.rs` |
| `POST /v1beta/models/{model}:generateContent` / `:streamGenerateContent` — Gemini (issue #494) | `gemini` | `chat` | the action in the URL selects it; a body `stream` field is refused with a pointer | `crates/server/tests/serving.rs`, `provider_destinations.rs` |
| `GET /v1/models` — priced catalog in OpenAI's shape | every protocol | — | — | `crates/server/tests/gateway.rs` |
| `POST /v1/tasks`, `GET`/`POST /v1/tasks/{id}` (`/cancel`) — async task relay (issue #503) | `tasks` | `event` price on the task model | n/a — submit then poll; the `poll-tasks` pass settles the hold | `crates/server/tests/tasks.rs` |

The price mode a surface bills under is fixed (`Surface::billing_mode`,
`crates/server/src/gateway/request.rs:51`): a model priced only `chat` is not served on
the input-only surfaces and vice versa — a mode mismatch fails closed rather than
reinterpreting a rate. `BillingMode` (`crates/core/src/billing.rs:33`) is `chat`,
`embeddings`, `rerank` or `event`; only `chat` and `event` meter an output side.

Usage lands in the normalized `UsageRecord` the adapters fold each protocol's report
into (`crates/core/src/adapters.rs`), then in the `request_operations` projection the
`/api/v1/operations` and `/api/v1/requests` reads serve
(`crates/server/tests/operations.rs`, `requests.rs`). An `Idempotency-Key` header
claims a turn before it runs: a same-key same-body retry replays the stored answer
instead of holding twice (issue #132, `crates/server/src/gateway/mod.rs`).

**Credential.** `require_key_gateway` (`crates/server/src/auth.rs:53`) accepts an
organization or member API key as `Authorization: Bearer`, `x-api-key` (Anthropic's
spelling) or `x-goog-api-key` (Gemini's). No session cookie: the surfaces take a
machine credential only. A member key's budget cap applies at admission; a signed
spend mandate (`POST /api/v1/org/keys/{keyId}/mandates`, issue #262) narrows a key's
models, cap and window per request.

## Metering surface

For events that never pass through the gateway — a service reports its own completed
or in-flight billable work. Credential: an owner-issued integration credential with
the `meteringWrite` scope, its selected customers explicit; scope and customer are
rechecked inside the authorization transaction
(`x-integration-scope` on each operation, `crates/server/tests/metering.rs`,
`integration_credentials.rs`).

| Endpoint | Effect | Test coverage |
| --- | --- | --- |
| `POST /api/v1/metering/holds` | freeze the declared usage bound at the channel's latest `event` price for `billableCode` | `a_hold_then_settle_charges_the_actual_usage` |
| `POST /api/v1/metering/settle` | settle a hold at actual usage; over-bound settles `capped` at the freeze | `a_settle_above_the_freeze_is_capped` |
| `POST /api/v1/metering/release` | let a hold go at zero, kind `released` | `a_release_lets_a_hold_go_and_a_late_settle_conflicts` |
| `POST /api/v1/metering/settlements` | one-shot: bill a completed event, no prior hold | `a_one_shot_settlement_bills_a_completed_event` |

`EventUsage` carries the token dimensions a price can itemize — input/output/cached/
reasoning tokens, `toolCalls`, image/audio/video token counts — plus `eventType`
(required), `serviceTier`, `endUser` and `tags` for attribution. Usage naming a
dimension the resolved price cannot bill is refused, never billed at zero
(`a_report_the_price_cannot_bill_is_refused`). Replay of an `idempotencyKey` answers
the original result; the same key with changed terms conflicts 409. An insolvent
customer answers 402, an unknown or unscoped one 404
(`an_insolvent_or_unknown_organization_is_refused`). The runnable client is
[examples/scoped-integration](../examples/scoped-integration/README.md), and the
server suite runs that actual client end-to-end
(`documented_python_client_reads_meters_replays_and_refuses_foreign_customer`).

## Payment rails

Payment authority is a **merchant**: an encrypted, immutable-versioned configuration
the business owns. `POST /api/v1/business/merchants` creates one
(`MerchantCreateRequest`: `rail` ∈ `hyperswitch`/`stripe`/`airwallex`, `mode`
`test`/`live`, `accountId`, `config`); `POST .../versions` rotates credentials by
appending a version; `POST .../status` sets checkout enablement. Rail-specific config
(`MerchantVersionInput`): Hyperswitch requires `publishableKey` + `sdkUrl`, Airwallex
requires `clientId` + `checkoutUrl`, and fields belonging only to another rail are
rejected. The env sets (`OXSUM_<RAIL>_MERCHANT_ACCOUNT_ID`/`_MODE`) bootstrap the
first merchant on an empty database.

A customer pays through an **order**: `POST /api/v1/orders` quotes
`displayAmountMinor`/`displayCurrency` and a `purpose`
(`walletTopup`/`bundle`/`planPeriod`/`invoiceRepayment`); the server derives the
ledger `amountMinor` at its own rate — client-supplied `amountMinor`/`rate` is a
schema rejection (`OrderCreateRequest` is `additionalProperties: false`).
`GET /api/v1/orders/{orderId}/checkout-session` re-reads or re-issues the provider
handles durably; only the signed webhook moves an order to `paid`.

| Rail | Checkout form the order serves | Signed webhook intake | Event kinds normalized | Test coverage |
| --- | --- | --- | --- | --- |
| `hyperswitch` | Unified Checkout SDK: `sdkUrl` + `publishableKey` + `clientSecret` — no hosted redirect | `X-Webhook-Signature-512`: hex HMAC-SHA-512 over the raw body, no timestamp window | `payment_succeeded`, `payment_failed`, `payment_cancelled`/`payment_expired`, `refund_succeeded`, `refund_failed`, `dispute_opened` | `crates/server/tests/hyperswitch.rs`, `merchant_events.rs` |
| `stripe` | hosted Checkout redirect URL | `Stripe-Signature` `t=…,v1=…`: HMAC-SHA-256 over `t.body`, 300 s freshness | `checkout.session.completed` (paid, non-subscription), `checkout.session.expired` | `crates/server/tests/stripe.rs` |
| `airwallex` | hosted payment page: intent + `return_url` | `x-timestamp` (Unix ms) + `x-signature`: HMAC-SHA-256 over `timestamp+body`, 300 s freshness | `payment_intent.succeeded`, `payment_intent.cancelled`/`expired`, `payment_intent.failed` | `crates/server/tests/airwallex.rs` |

Signature verification precedes normalization, exactly as coded in
`crates/integrations/src/merchant_events.rs` (`verify`); the header names live in
`crates/server/src/routes/merchant_events.rs:37`. Two intake paths verify the same
bytes the same way:

- the versioned, merchant-scoped callback `POST /api/v1/webhooks/merchants/{merchantId}/versions/{versionId}` — path coordinates select the signing configuration, so the payload cannot pick a secret;
- the environment callback `POST /api/v1/webhooks/{stripe|airwallex|hyperswitch}` — resolves the env-bootstrapped merchant and tries its retained versions (`crates/server/src/routes/merchant_events.rs`, `environment`).

The verified event lands in `oxsum.merchant_event_inbox`
(`receive_merchant_event`, `crates/core/src/merchant_events.rs:302`):

- dedup key is `(merchant_id, event_id)`; a replayed identical delivery answers 200 without duplicating (`a_replayed_webhook_credits_once` in each rail suite);
- the same event id with different payload bytes conflicts `409` — "merchant event ID was reused with different content";
- kinds nothing recognized become `unsupported` → state `ignored`, acknowledged without financial effect;
- recognized events must match the pinned payment's merchant, version, customer, provider account, mode, and — on success — amount and currency exactly, else `rejected` with the named reason; a payment the merchant never pinned stays `pending` (`unknownPayment`) for recovery rather than being dropped;
- missing local payment/customer metadata is derived only from an already-bound provider payment reference inside the signed merchant/version. Supplied conflicting or malformed coordinates are not replaced. A callback arriving before its reference stays `pending` (`paymentReferencePending`) and recovery re-evaluates it. The original normalized event and raw digest remain immutable; correlation is a derived read of immutable bindings (#759);
- `accepted` → `applied` when the fulfillment actually moves the wallet; `pending` and `accepted` rows are what the recovery sweep re-reads;
- `GET /api/v1/business/merchants/{merchantId}/events` is the redacted staff read — coordinates, state and rejection reason only, never raw payload or secret.

New order refunds send their distinct `refund_id`, actual `order_id`/
`payment_id` and `organization_id` to the switch. Historical refunds that only
carried a refund identifier can still correlate through the pinned provider
payment reference. This repairs scoped intake for the local Hyperswitch
v1.129.0 response shape; it does not establish genuine PSP callback delivery.
Provider request idempotency and ambiguous-outcome recovery are tracked in
#760 and remain separate acceptance requirements.

Two funding paths carry no webhook inbox. The **x402 rail**
(`POST /api/v1/topups/x402`) credits a signed payment authorization directly —
verify/settle delegated to a facilitator sidecar, or to the stand-in verifier for a
test deployment (`crates/server/tests/x402.rs`); it is not a merchant rail and mints
no order. And **manual receipts**
(`POST /api/v1/admin/organizations/{id}/manual-receipts`, deployment token;
`POST /api/v1/business/orders`, staff session) record out-of-band money as a
`source: manual` fulfilled order. The legacy customer `POST /api/v1/topups` is
retired — it always answers 403; every customer-crediting path is an order or an
admin receipt.

Rails are environment-gated: each `OXSUM_<RAIL>_*` set is all-or-nothing, a
half-configured rail refuses to boot, and an unconfigured rail answers 503 on its
intent POST and 404 on its inbound webhook (`an_unconfigured_deployment_refuses_cleanly`
in each suite). `OXSUM_RAILS` declares which configured rails a deployment means to
run; a declaration the credentials disagree with fails the boot
(`deploy/operator.env.example`).

## Authentication integrations

| Integration | Configures | Endpoints | Required for | When absent |
| --- | --- | --- | --- | --- |
| Password | — | `POST /api/v1/auth/register`, `/login`, TOTP under `/auth/totp*` | always available | — |
| GitHub OAuth | `OXSUM_GITHUB_CLIENT_ID`/`_SECRET` + `OXSUM_PUBLIC_URL` | `GET /api/v1/auth/oauth/github`, `.../callback` | optional login method | no button; endpoints 404 |
| OIDC (issue #504) | `OXSUM_OIDC_PROVIDERS` JSON, `OXSUM_PUBLIC_URL` | `GET /api/v1/auth/oauth/oidc:{name}`, `.../callback` | optional enterprise SSO | no OIDC mount |
| Turnstile (issue #154) | `OXSUM_TURNSTILE_SITE_KEY`/`_SECRET_KEY` | `turnstileToken` field on `register` and `invitations/redeem` | required on those two endpoints when set | endpoints run unchecked |
| Device flow | — | `/api/v1/device/code`, `/request`, `/authorize`, `/token` | CLI/agent sign-in | — |
| SMTP mailer | `OXSUM_SMTP_URL`, `OXSUM_MAIL_FROM`, `OXSUM_PUBLIC_URL` | verification and password-reset mail | self-registration, invitations, resets | no mail sent; resend answers 503 |

Business owner setup is not an integration but a gate every flow above sits behind:
`POST /api/v1/admin/business/setup-tokens` (deployment token) → `/api/v1/business/setup`
or `/setup/register` (the invited owner's session), then
`POST /api/v1/business/registration` selects invite-only or open customer policy
(`crates/server/tests/business_setup*.rs`). Self-registration answers
`FORBIDDEN` until the owner opens it.

## Explicitly not supported

Each refusal is a behavior a caller can test, not a gap to assume around.

| Not served | Behavior | Where it stands |
| --- | --- | --- |
| `/v1/images`, `/v1/audio/*`, `/v1/fine_tuning`, `/v1/batches`, `/v1/moderations`, `/v1/assistants`, threads | not routed — falls through to 404 | no surface, no adapter; the `event` price mode can *meter* these dimensions (tool/image/audio/video token fields on `EventUsage`) when a service reports them, but the gateway forwards no such request |
| Batch/embeddings bulk API | not routed | `/v1/embeddings` is the synchronous single call only |
| Provider-account import OAuth | not wired | issue #506 — deferred; named here so no reader assumes coverage |
| Scope beyond `customersRead`/`meteringWrite` | schema rejection — `IntegrationScope` is a closed enum | `crates/server/openapi.yaml` |
| Wildcard customer on an integration credential | `customerIds` is 1–1000 explicit UUIDs, no wildcard | same schema |
| `stream: true` on `/v1/embeddings` or `/v1/rerank` | parameter refusal — "this surface does not stream" | `crates/server/src/gateway/request.rs:199` |
| Client-set ledger amount on an order | schema rejection — `OrderCreateRequest` has no `amountMinor`/`rate` fields | [unsupported.md](unsupported.md) |
| Legacy `oxs-svc-` credentials | 401 — disabled | `examples/scoped-integration/README.md` |

## Verification SDK and examples

The `verify-oxsum-bill` npm package (`web/verify-sdk`, packed locally — not yet
published) exports the wasm-backed verifiers: `verifyBill`, `verifySignedHead`,
`verifyConsistency`, `verifyWitnesses`, `verifyStatementAnchor`, `verifyUsageReport`,
`verifyPriceHead`, `verifySaleHead`, `verifyEvidenceBundle`, and `listBundleManifests`, the pure function that extracts a bundle's
`{kind, id, digest}` manifest rows for inspection. All take and return JSON the
endpoints serve; none fetches anything.

| Example | Proves | Runs against |
| --- | --- | --- |
| [examples/verify-consumer](../examples/verify-consumer/README.md) | the packed tarball verifies captured genuine evidence offline — bill, signed head, consistency, v2 evidence bundle — and rejects tampered inputs | nothing; fixtures only (`scripts/check-verify-consumer.sh`) |
| [examples/scoped-integration](../examples/scoped-integration/README.md) | an `oxs-int-` credential reads its selected customer and meters an event — and refuses what its scope does not cover | a running deployment; exercised by `integration_credentials` |
| [examples/scoped-management](../examples/scoped-management/README.md) | mint an `oxs-int-` credential, read its selected customer, bill one event through `/api/v1/metering/settlements`, replay it idempotently, and read the operation back under deployment authority — asserting the scope's own refusals | a running deployment |
| [examples/consumption](../examples/consumption/README.md) | the full customer journey over the documented endpoints: invite → register → manual-receipt top-up → metered call → usage/bill read-back → evidence bundle → offline verify | a running deployment |
| [examples/webhook-tools](../examples/webhook-tools/README.md) | merchant webhook signature, replay dedup and retry-storm single-credit semantics | a running deployment |
| `scripts/verify-examples.sh` | every example above, end-to-end, on an owned scratch database and loopback listener | [isolated runner prerequisites and artifact options](development.md#developer-examples-and-the-fresh-install-gate-617) |

The evidence endpoints the examples and SDK consume:
`POST`/`GET /api/v1/org/evidence-bundles`,
`GET /api/v1/org/evidence-bundles/{bundleId}`,
`POST .../{bundleId}/share`, the public `GET /api/v1/evidence/{token}` (an expired,
revoked, exhausted or unknown token all answer 410 indistinguishably), and
`GET /api/v1/org/statements/{statementId}/evidence` for the statement-scoped bundle
(issue #616; proofs in `crates/server/tests/evidence_bundles.rs`).

### Hyperswitch v1 retry identity (#760)

The pinned v1.129.0 API uses body `payment_id` and `refund_id`, not an assumed
`idempotency-key` header, to identify creates. Oxsum persists the local UUID
identity policy before I/O and first GETs that exact ID with the pinned
merchant/version credentials. The precise `HE_02` not-found response permits a
new create; generic proxy 404s, authentication errors and malformed responses
do not. Duplicates and dropped responses recover by GET plus exact identity,
amount, currency and local metadata checks. Refund responses omit merchant ID
in this version; credential scope and the pinned payment ID establish it, and
a supplied conflicting account is rejected. No read alone settles the wallet.

The upgrade leaves old rows marked as historical. Existing handles are read
only; historical unbound operations cannot silently acquire a fresh external
identity. New workers claim a bounded durable attempt lease; a missing terminal
event retains the refund amount after automatic retries stop. `IR_13` is a
verified pre-insert rejection in this version, but it can release only on the
first attempt, before any earlier uncertain call. Signed refund events must
match the order, refund identity, customer, amount and currency. An unfamiliar
refund ID cannot select another partial refund on the same payment.

This guarantee covers purpose-scoped payment orders and merchant-bound checkout.
The older deployment-admin deposit reversal adapter remains a distinct legacy
path; its retry conversion is tracked separately in #763 and is not certified by these
order tests. Real PSP certification remains separate from local switch evidence.

### Legacy Hyperswitch reversal recovery (#763)

A deployment-admin deposit reversal is distinct from an order refund. New
reversals persist one UUID in the v1.129 `refund_id` body and pin the original
payment merchant/version before I/O. Exact `HE_02` absence permits create; other
HTTP errors remain unknown, and duplicates/lost responses are read back. Every
handle must match payment, amount, currency and local metadata. The client uses
the stored destination policy and bounded requests, never a current environment
credential substituted for the original merchant version. The signed callback
must additionally match customer and refund identity. An unbound legacy callback
is retained but cannot assert a terminal result for a guessed deposit.

Historical reversals preserve original metadata without treating an old HTTP
success as authenticated terminal evidence. Known pinned handles are read-only;
unknown old outcomes are fenced. Five leased attempts bound recovery. Exhaustion
retains unknown/pending state, and only an exact signed outcome settles it.
Local fixtures prove retry and accounting behavior, not real PSP certification.
