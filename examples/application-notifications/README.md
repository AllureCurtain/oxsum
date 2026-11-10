# Signed application notifications

A standard-library Python receiver stores original signed bytes in its own SQLite
inbox and reads current scoped oxsum state. It never applies a balance, entitlement
or archive state from a notification. Duplicate IDs have one durable inbox row;
old signals fetch current facts rather than overwriting newer state. Intake is
acknowledged after SQLite commit; failed reconciliation stays pending.

In Business → Application access, select the application and create a notification
endpoint. Public HTTPS is the default. For this isolated loopback example, explicitly
approve `http://127.0.0.1:8081/events` with the internal policy and exact allowed
IP `127.0.0.1`. Keep the returned signing secret: it is shown once. Endpoint replay
cannot retrieve it. Create a replacement and disable the old target to rotate.

Supply the secrets without putting them in a command argument or tracked file:

```sh
export OXSUM_NOTIFICATION_SECRET='<one-time whsec- secret>'
export OXSUM_INTEGRATION_SECRET='<application management credential>'
python3 examples/application-notifications/receiver.py \
  --database /absolute/private/path/notifications.sqlite \
  --application-id '<owner-created application UUID>' \
  --base-url http://127.0.0.1:3000 --port 8081
```

This local receiver binds loopback only. Its database directory must already exist.
For the subscribed signal types, separately grant the matching current-state reads:
`entitlementsRead`, `customerKeysWrite`, `checkoutWrite`, `evidenceRead`, `billingRead`.
`notificationsRead` exposes endpoint and delivery metadata; it does not imply any
of those other scopes. A missing read grant keeps the inbox pending rather than
pretending reconciliation succeeded. Select only events the runtime needs.

The billing response contains a bounded ledger window; it is not a complete
statement history. Key and evidence pages are bounded. Persist your own customer
mapping and make initial scoped reads before relying on signals; endpoint creation
has no historical replay guarantee. Natural time boundaries (credential/key expiry,
period end) also require a current read on use. No notification is a payment receipt,
complete accounting proof or independently trusted witness. Downloaded evidence
still needs the documented verifier and independently retained trust inputs.

The sample rejects wrong/old HMAC, altered bodies, header/envelope mismatch and
wrong application IDs before intake. It refuses redirects and environment proxies
when sending its integration credential. This example is a receiver, not a full
application or production public ingress; the separate N21 adoption example remains.

```sh
python3 -m unittest discover -s examples/application-notifications -p 'test_*.py'
```

The Rust HTTP fixture sends actual signed bodies to a loopback receiver, checks
unchanged retry bytes and passes captured Rust output to this Python receiver for
independent signature verification and durable deduplication. These tests contact
no external service. Disable a target to stop new enqueue and unclaimed delivery;
a delivery already claimed by a worker may still finish. Delivery metadata is
available from the application interface or the owner UI, and only the owner may
start a fresh retry cycle for a failed delivery.
