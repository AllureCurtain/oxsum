# Consumption example

A Node walkthrough (`consumption.mjs`, Node ≥ 20.16) that drives
the whole customer journey against an explicitly selected **isolated test** deployment and asserts each
step out loud. It depends on the `verify-oxsum-bill` package for the final
offline check — install the packed tarball (`scripts/check-verify-consumer.sh`
builds one, or `npm install ../../web/verify-sdk` after a build).

## What it does, in order

1. **Owner setup.** Mints a setup token under the deployment's admin token
   and registers the business owner — or, on an already-initialized
   deployment, logs the owner named by `OXSUM_OWNER_EMAIL`/`OXSUM_OWNER_PASSWORD`
   in. An initialized deployment requires both credentials explicitly; there is no seeded-password fallback.
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
   `POST /api/v1/business/channels/{name}/prices`. It publishes a sale book and explicitly assigns it to this customer.
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

The script always uses the local stub model it priced; it cannot select a paid model. The run's last lines export
`OXSUM_CUSTOMER_ID`/`OXSUM_CUSTOMER_KEY` (and `OXSUM_OWNER_*` when it
registered the business itself) so a caller — `scripts/verify-examples.sh`
does — can feed the fresh customer to the scoped-management example.

A real card rail is *not* what this example covers: `POST /api/v1/orders` is
the customer checkout path, but its fulfillment needs a provider callback,
which an isolated script cannot produce — the manual receipt is the honest
substitute and is documented as the stub. Payment-order coverage lives in
[`../webhook-tools`](../webhook-tools/README.md), which drives a merchant's
signed event intake end to end.

The owner/customer/funding bootstrap is shared with `demo/demo.py` through `bootstrap.mjs`. For a fresh database, an SDK stream and verification of that same immediate settlement after the server stops, use the README demo command. The closed-day bundle below is empty for a newly created customer and does **not** verify the turn just made.

## What a run proves

- Invitation → registration → first key is atomic and key-secret-once.
- The consumption key alone reaches `/v1/*`, `/api/v1/billing-records`,
  safe catalog and evidence reads. Balance requires the customer session.
- The admin token and the customer credential are different authorities: the
  customer could not have minted its own membership or funds.
- The evidence bundle verifies offline against the key the bundle publishes —
  on a deployment where the operator pins its signing key out of band, the
  same call takes that key instead of the bundle's copy.

`scripts/verify-examples.sh` runs this script against a clean deployment and
counts its assertions.
