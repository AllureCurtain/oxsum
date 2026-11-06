# Webhook tools

Three runnable Node scripts (no dependencies, Node ≥ 20.16) that exercise the
**versioned merchant event intake** —
`POST /api/v1/webhooks/merchants/{merchantId}/versions/{versionId}` — the
durable, scoped callback surface a provider's signed deliveries land on.

Each tool creates a merchant through the business API, signs real provider
event bytes under the rail's documented scheme, and asserts what the intake
does with them. Nothing contacts a payment provider; the signatures are what
a provider would send, verified against the merchant version's stored
`webhookSecret`.

## The signature schemes (as implemented)

| Rail | Header | MAC | Signed bytes | Freshness |
| --- | --- | --- | --- | --- |
| `hyperswitch` | `X-Webhook-Signature-512` | HMAC-SHA512 hex | raw body | none — `event_id` dedup is the replay defense |
| `stripe` | `Stripe-Signature` `t=…,v1=…` | HMAC-SHA256 hex | `"{t}.{body}"` | ±300 s on the versioned intake |
| `airwallex` | `x-timestamp` + `x-signature` | HMAC-SHA256 hex | `timestamp + body` | ±300 s |

The versioned route verifies the signature *before* parsing — a bad signature
is `401` and an unknown merchant/version coordinate is `404`, whatever the
body holds.

## The tools

- **`sign-and-replay.mjs`** — sign an event, post it, read the merchant's
  inbox. An unrecognized payment parks at `pending`/`unknownPayment` (kept
  for recovery, never credited); exact replay answers 200 without
  duplicating; a mis-signed delivery is 401; unknown coordinates are 404.
- **`dedup-check.mjs`** — post the same provider `event_id` over different
  bytes; asserts `409 CONFLICT` and that the original bytes' row survives
  alone.
- **`retry-storm.mjs`** — fire N concurrent identical signed callbacks;
  asserts all answer 200 and the inbox holds exactly one row.

## Running them

Against a running local server (`cargo run -p oxsum-server` after
`docker compose up -d`). The credentials name a **business owner** — the
merchant create lives under `/api/v1/business/*`. A seeded demo database has
no business of its own, so run [`../consumption`](../consumption/README.md)
first (it registers an owner and prints the login), or supply your
deployment's owner:

```sh
OXSUM_BASE_URL=http://127.0.0.1:3000 \
OXSUM_OWNER_EMAIL=<a business owner's email> \
OXSUM_OWNER_PASSWORD=<its password> \
node sign-and-replay.mjs
node dedup-check.mjs
node retry-storm.mjs --n 16
```

`--rail hyperswitch` (default) is the fully documented rail; `--rail stripe`
and `--rail airwallex` exercise their signature schemes the same way — the
event-kind normalization differs per rail (see `docs/integrations.md` for the
per-rail event matrix), but signature/dedup semantics are shared.

To watch the `accepted → applied` walk a bound payment needs: create a
payment order through a deployment-configured rail (`OXSUM_<RAIL>_MERCHANT_*`
env bootstrap makes the rail's merchant selectable on `POST /api/v1/orders`),
then post a signed `succeeded` event whose `payment_id` names the order id —
the inbox walks `accepted` and the order's fulfillment runs under the
order-derived ledger key. That flow needs a configured rail, so it is part of
`scripts/verify-examples.sh` on a payment-profile deployment, not the bare
example.

## Refusals the tools prove

| Behavior | Answer | Why |
| --- | --- | --- |
| Unsigned/mis-signed delivery | `401` | signature verifies before parsing |
| Unknown merchant/version path | `404` | coordinates select the signing secret; a guessed one names nothing |
| Same event id, different bytes | `409` | the digest check refuses a mutation claiming the same id |
| Event for an unknown payment | `200`, state `pending`/`unknownPayment` | receipt is not fulfillment; the row is retained for recovery |
| Unsupported event kind | `200`, state `ignored`/`unsupportedEvent` | acknowledged with no financial effect |
| Cross-merchant replay | `404`/rejected | events are scoped `(merchant_id, event_id)`; a foreign merchant cannot claim the id |
