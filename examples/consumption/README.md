# Consumption example

A single-file Node walkthrough (`consumption.mjs`, Node ≥ 20.16) that drives
the whole customer journey against a **live** deployment and asserts each
step out loud. It depends on the `verify-oxsum-bill` package for the final
offline check — install the packed tarball (`scripts/check-verify-consumer.sh`
builds one, or `npm install ../../web/verify-sdk` after a build).

## What it does, in order

1. **Owner setup.** Mints a setup token under the deployment's admin token
   and registers the business owner — or, on an already-initialized
   deployment, logs the owner named by `OXSUM_OWNER_EMAIL`/`OXSUM_OWNER_PASSWORD`
   in (defaulting to the seeded demo's `admin@oxsum.local`/`admin-password`).
   `/api/v1/business/*` calls take the owner's session in business context.
2. **Invite a customer.** `POST /api/v1/business/customer-invitations`
   reserves the customer UUID and returns the one-shown token.
3. **Redeem.** `POST /api/v1/invitations/redeem` creates the customer, its
   owner membership and the first API key — the `oxs-` secret answered once.
4. **Top up.** `POST /api/v1/admin/organizations/{id}/manual-receipts` — the
   deployment-authority call that stands in for a payment rail on an
   isolated deployment. Customers cannot assert money arrived themselves.
5. **Provision a reachable upstream.** The seeded demo channel points at an
   unreachable provider by design (seeded traffic must not leak), so the
   script serves its own `node:http` stub on 127.0.0.1 and registers a
   channel to it — `POST /api/v1/business/channels` with an explicit
   `internal` destination policy — plus a chat price through
   `POST /api/v1/business/channels/{name}/prices`.
6. **Call the gateway.** `POST /v1/chat/completions` under the customer's
   `oxs-` key; asserts `usage` comes back.
7. **Read back.** `GET /api/v1/balance` (session) shows the settled charge;
   `GET /api/v1/billing-records` (key) shows the turn's row.
8. **Evidence.** `POST /api/v1/org/evidence-bundles` over the open day is the
   documented `400 VALIDATION_ERROR`; over the just-closed day the bundle
   mints, and `GET /api/v1/org/evidence-bundles/{id}` downloads it.
9. **Verify offline.** `verifyEvidenceBundle` checks the downloaded bytes
   with `fetch` disabled — the check cannot call the server back.

## Running it

```sh
npm install /path/to/verify-oxsum-bill-0.1.0.tgz
OXSUM_BASE_URL=http://127.0.0.1:3000 \
OXSUM_ADMIN_TOKEN=<the deployment's admin token> \
node consumption.mjs
```

`OXSUM_MODEL` pins the model the gateway call asks for; unset, the script
uses the stub model it priced. The run's last lines export
`OXSUM_CUSTOMER_ID`/`OXSUM_CUSTOMER_KEY` (and `OXSUM_OWNER_*` when it
registered the business itself) so a caller — `scripts/verify-examples.sh`
does — can feed the fresh customer to the scoped-management example.

A real card rail is *not* what this example covers: `POST /api/v1/orders` is
the customer checkout path, but its fulfillment needs a provider callback,
which an isolated script cannot produce — the manual receipt is the honest
substitute and is documented as the stub. Payment-order coverage lives in
[`../webhook-tools`](../webhook-tools/README.md), which drives a merchant's
signed event intake end to end.

## What a run proves

- Invitation → registration → first key is atomic and key-secret-once.
- The consumption key alone reaches `/v1/*`, `/api/v1/billing-records`,
  `/api/v1/balance` reads — no session needed for the machine surface.
- The admin token and the customer credential are different authorities: the
  customer could not have minted its own membership or funds.
- The evidence bundle verifies offline against the key the bundle publishes —
  on a deployment where the operator pins its signing key out of band, the
  same call takes that key instead of the bundle's copy.

`scripts/verify-examples.sh` runs this script against a clean deployment and
counts its assertions.
