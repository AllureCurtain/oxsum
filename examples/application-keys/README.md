# Application customer keys

Use an existing application namespace and provisioned customer. An owner issues
an application credential with `customerKeysWrite` once from the business Staff
page; the runtime never holds an owner password/session. The credential must
remain valid for more than five minutes. The selected model must be one the
customer is otherwise permitted to use.

```bash
OXSUM_BASE_URL=http://127.0.0.1:3000 \
OXSUM_APPLICATION_CREDENTIAL='<scoped secret>' \
OXSUM_APPLICATION_CUSTOMER_ID='<application customer UUID>' \
OXSUM_APPLICATION_MODEL='<allowed model>' \
OXSUM_APPLICATION_KEY_STATE='/path/to/task-owned/key-operation.json' \
node examples/application-keys/application-keys.mjs
```

Use task-owned fixtures. The walkthrough issues a five-minute key with a
1,000-minor cumulative budget, two requests/minute and one concurrent request.
It checks exact replay and secret-free metadata, then revokes the key. It does
not print secrets, call an upstream, add funds or contact another customer.
A lost creation response is retried once with the original identity so the
known key can be revoked. The state file records the exact request and revocation identity before any
network action and contains no secret. Preserve and reuse it if the listener
stays unreachable or the process exits; a completed record refuses to create a
second key. Use only one process per state file. A new file starts a deliberately
new operation; do not treat an uncertain response as a failed effect.

Management-credential revocation does not silently revoke customer consumption
keys already issued. Those keys keep their own expiry/revocation; archived or
suspended customers remain unable to consume. Full embedded J18, entitlement,
checkout/evidence scopes, consent linking and signed notifications are separate.
