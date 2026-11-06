# Scoped-management example

A single-file Node walkthrough (`scoped-management.mjs`, no dependencies,
Node ≥ 20.16) that runs the whole scoped-credential journey against a live
deployment and asserts each step out loud. It is the scripted counterpart of
[`../scoped-integration`](../scoped-integration/README.md)'s Python client:
that one is a *client* you point at events; this one is a *walkthrough* that
proves the surface end to end.

## What it does

1. Logs in as the business **owner** — `POST /api/v1/auth/login`, session
   cookie. Business calls take a personal session, never an API key.
2. Ensures the channel carries a `mode: event` price for the billable code —
   `POST /api/v1/business/channels/{name}/prices`. On by default
   (`OXSUM_APPEND_EVENT_PRICE=0` disables): a seeded deployment has no event
   prices, and an unpriced code refuses the metering call. On a deployment
   that already prices the code the append is a no-op version bump — set the
   variable to reuse exactly what is there.
3. Mints an **integration credential** —
   `POST /api/v1/business/integration-credentials` with scopes
   `["customersRead","meteringWrite"]`, exactly one `customerIds` entry and a
   7-day expiry. The `oxs-int-` secret is answered once, in this response.
4. Reads the selected customer —
   `GET /api/v1/business/customers/{organizationId}` with the credential as
   `Bearer`. Asserts the scope refuses the owner-only credential-list
   endpoint in the same breath.
5. Bills one completed event — `POST /api/v1/metering/settlements` with a
   declared `usage` (`inputTokens` units on a `mode: event` billable code).
   Asserts `chargedMinor` and `kind: "usage"`.
6. Replays the same `idempotencyKey` — asserts the original `settlementId`
   comes back instead of a second charge.
7. Reads the operation back under **deployment authority** —
   `GET /api/v1/admin/operations?organizationId=…` with `OXSUM_ADMIN_TOKEN`
   finds the `surface: "metering"` row; without the token the step prints
   SKIP. The service credential itself is refused `GET /api/v1/operations`.
8. Checks the documented scope boundary: customer-facing operation lists are
   scoped to the key that booked them, so the customer's own `oxs-` key —
   supplied as `OXSUM_CUSTOMER_KEY` — correctly does *not* list the
   service-written row. A member session or the admin read is the
   customer's-side view of it.

## Running it

Against a seeded local server (`docker compose up -d`,
`OXSUM_DEMO_SEED=true cargo run -p oxsum-server -- seed` once, then
`cargo run -p oxsum-server`) — note the seed has no business of its own, so
run [`../consumption`](../consumption/README.md) first or point
`OXSUM_OWNER_*` at an existing business owner:

```sh
OXSUM_BASE_URL=http://127.0.0.1:3000 \
OXSUM_OWNER_EMAIL=<a business owner's email> \
OXSUM_OWNER_PASSWORD=<its password> \
OXSUM_CUSTOMER_ID=<a customer organization uuid> \
OXSUM_CUSTOMER_KEY=<the customer's oxs- key> \
OXSUM_ADMIN_TOKEN=<the deployment's admin token> \
node scoped-management.mjs
```

`OXSUM_EVENT_CHANNEL` (default `demo`) and `OXSUM_EVENT_CODE` (default
`demo-event`) name the channel and `mode: event` price the metering call
bills under. `OXSUM_EVENT_UNITS` (default `25`) is the `inputTokens` count;
`OXSUM_EVENT_PRICE_PER_MILLION` (default `4000`) is the appended price.

The script refuses a non-loopback `http://` base URL and any URL carrying
credentials, query or fragment — run it against an isolated deployment only;
the metering call moves real customer credits.

## Expected failures worth knowing

- Mint without `integrationsManage` → `403`.
- Credential on an unselected customer → `404` (`customersRead`) / `404`
  (`meteringWrite`) — a foreign customer is *not found*, never *forbidden*,
  so the allowlist cannot be probed.
- Changed body under a used `idempotencyKey` → `409`.
- Usage naming a dimension the price cannot bill → `400`, never partial.
- An insolvent customer → `402`.

`scripts/verify-examples.sh` runs this script as one step of the fresh-install
gate; see that script for the clean-deployment shape it expects.
