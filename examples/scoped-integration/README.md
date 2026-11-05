# Scoped integration client

Use an isolated local oxsum deployment first. The standard-library Python client
reads one allowed customer or explicitly bills a completed external event. It
never mints credentials, sends an upstream model request, follows redirects or
prints the secret. The metering command moves customer credits and requires an
explicit `--confirm-metering` flag.

In a current owner session, select business context, then call
`POST /api/v1/business/integration-credentials` with a body such as:

```json
{
  "idempotencyKey": "issue-service-a",
  "name": "service-a",
  "scopes": ["customersRead", "meteringWrite"],
  "customerIds": ["CUSTOMER_UUID"],
  "expiresAt": "A_FUTURE_RFC3339_TIME_WITHIN_365_DAYS",
  "reason": "Meter service-a events for the selected customer"
}
```

Save the returned secret once in your secret manager. Exact creation retry returns
`secret: null`; changed terms return 409. Select only necessary scopes/customers;
there is no wildcard. Do not use a customer key or the deployment token. Legacy
`oxs-svc-` credentials are disabled.

Supply `OXSUM_INTEGRATION_SECRET` through your secret manager or an interactive
read (do not save it in shell history). The example-only environment variable is
not server configuration. From this directory:

```sh
python3 client.py customer CUSTOMER_UUID
python3 client.py meter CUSTOMER_UUID --channel event-channel --code example-event \
  --key service-a-event-001 --event-id external-label-001 --units 25 --confirm-metering
```

The channel must have an `event` price for `example-event`, with input-token units
priced for this example, and the selected customer must have authorized funds.
Choose the idempotency key before sending and persist it with the event. If the
connection fails, resend identical terms and the same key/credential: a changed
operation/body conflicts. Never choose a fresh key to repeat an uncertain bill.
The returned `settlementId` identifies its receipt. The hold/settle form is in
[the API guide](../../docs/api.md); save the returned opaque hold key.

Expected negative cases: wrong scope 403, foreign customer/hold 404,
expired/revoked/legacy secret 401, changed-body replay 409, insufficient funds 402.
`eventId` and `endUser` are labels inside the selected customer, not authorization.
Rotate by owner issuance, then `POST /business/integration-credentials/{id}/revoke`
with a new idempotency key and reason. New credentials cannot close old credentials'
holds; use trusted recovery for orphaned holds.

The server integration suite exercises this actual client against a loopback
production router and isolated PostgreSQL, including read, meter, exact replay
and foreign customer denial. It makes no paid provider or payment calls.
