# Isolated Hyperswitch test stack

This is a disposable **local simulation**, not a payment-processor account or
a production deployment. A real Stripe test key is a separate prerequisite.
The `paypal_test` connector is Hyperswitch's in-process dummy processor; its
success/refund responses do not certify PayPal, Stripe or any other PSP.

## Prepare and run

Prerequisites: Docker Compose v2, Git, Python 3.11+ and network access to the
pinned source/images. Run from the repository worktree:

```bash
./infra/hyperswitch/setup.sh
python3 infra/hyperswitch/stack.py check
python3 infra/hyperswitch/stack.py up
python3 infra/hyperswitch/merchant.py provision
python3 infra/hyperswitch/merchant.py inspect
```

`versions.json` pins the source commit/tag, every image's manifest digest and
the Diesel migration binary/checksum. The supported tool platforms are Linux
arm64/amd64. This compatibility set was brought up on arm64; it does not prove
the separate oxsum release's amd64 capacity/restore gate. The router image
reports `v1.129.0-dirty-cf913c5` from upstream; the retained immutable image
digest, not a claim of a pristine upstream build, identifies its bytes.

`setup.sh` clones the exact source under ignored `.repo/`. It refuses a changed
commit or dirty checkout and never resets it. Runtime configuration is rendered
from that same snapshot. Migration uses verified Diesel 2.3.5 without schema
code generation or editing upstream migrations. The router uses the snapshot's
supported Superposition file fallback; periodic refresh warnings are expected
because this fixture deliberately has no remote policy service.

`.runtime/` is mode 0700, with private JSON/config/env files written atomically
as 0600. It retains random database, encryption, signing and admin keys plus a
random `oxsum-hs-<hex>` Compose project. `merchant.json` retains a new merchant,
default profile, one-time API key, publishable key, webhook secret and dummy
connector identity. Provisioning reconciles existing state before creating
anything; it refuses to silently replace a lost one-time key. Keep these files
with the volumes. Never print, commit or copy them into an issue.

There is no shared demo password or automatically enrolled Control Center
user. Admin API operations use the generated local admin key; SDK/payment and
connector operations use the merchant key. The Control Center remains private
and its interactive account enrollment is separate from this API-provisioned
merchant.

| Service | Local address | Persistent state |
| --- | --- | --- |
| Router | `http://127.0.0.1:18080` | `router_files` |
| SDK | `http://127.0.0.1:19050/HyperLoader.js` | reproducible pinned image |
| Control Center | `http://127.0.0.1:19000` | state in PostgreSQL |
| Mailpit UI | `http://127.0.0.1:8025` | `mail_data` |
| Mailpit SMTP | `127.0.0.1:1025` | messages in `mail_data` |
| PostgreSQL 17 | internal Compose network only | `pg_data` at `/var/lib/postgresql/data` |
| Redis | internal Compose network only | `redis_data`, AOF enabled |

Ports can be changed together in the private state's `ports` and corresponding
URL fields before preparing it again. Inherited `HS_*`/`COMPOSE_*` variables
cannot redirect script operations to another project or mutable image.
`stack.py up` recreates the services so regenerated mounted configuration is
actually loaded, while retaining their named volumes.

## Oxsum API smoke

Build the current oxsum binary separately, then pass its explicit path:

```bash
python3 -m unittest discover -s infra/hyperswitch -p 'test_*.py'
python3 infra/hyperswitch/smoke.py --binary /absolute/path/to/oxsum
```

The smoke requires the existing local oxsum development PostgreSQL container
(`oxsum-postgres-1`, overridable by `--postgres-container`) at loopback port
5432 with the repository's test credentials. It creates a uniquely named
database, refuses to fall back if creation fails, launches the specified
binary in a private empty-env directory and removes only that database at exit.
The listener inherits no oxsum/provider/payment configuration from the caller.
No real model is configured or called.

It initializes owner/customer consent, creates an exact USD 2 order (1 credit
= USD 1), checks retry identity and pending balance, confirms through the local
dummy card processor, then injects a **locally signed** switch-result event.
It checks bad signatures, duplicate events, worker fulfillment and a partial
refund. The complete scoped-refund smoke requires the correlation fix in #759;
the previous baseline intentionally fails on the real switch response. The
provider retry/unknown-outcome defect tracked in #760 is not covered by this
one-shot success claim. The API-only fixture uses an unserved HTTPS SDK placeholder when no
real sandbox HTTPS origin exists; it never opens a browser or claims checkout
acceptance. Sanitized results and private diagnostics remain under
`.runtime/smoke-<id>/`. Switch test payments remain in this task's own volumes.

## HTTPS SDK, API and callback topology

Oxsum requires a public HTTPS SDK URL. A browser cannot use a server's localhost
as its API, and an HTTPS page must not load an HTTP SDK. Use separate temporary
HTTPS origins for SDK assets and client API, plus the fictional oxsum app's
exact scoped callback. Keep Control Center, mail, database, merchant/admin API
and private files off the public routes.

`public_proxy.py` supplies loopback-only restricted front doors for a tunnel:

```bash
python3 infra/hyperswitch/public_proxy.py sdk --port 19150 --target-port 19050
python3 infra/hyperswitch/public_proxy.py api --port 18180 --target-port 18080
# In separate terminals, with an explicitly installed/pinned cloudflared:
cloudflared tunnel --no-autoupdate --url http://127.0.0.1:19150
cloudflared tunnel --no-autoupdate --url http://127.0.0.1:18180
```

The API proxy admits SDK client routes with publishable keys and rejects admin
or secret merchant keys. The asset proxy admits only GET/HEAD static assets.
Neither logs URLs or headers. Do not tunnel the raw router port. Stop each
tunnel/proxy after acceptance. A quick tunnel allocation is not proof of a
working connection; verify its public assets and SDK API traffic.

With actually reachable origins, configure both together:

```bash
./infra/hyperswitch/setup.sh \
  --router-origin https://api-fixture.example \
  --sdk-origin https://sdk-fixture.example
python3 infra/hyperswitch/stack.py up
```

This sets the SDK's `ENV_BACKEND_URL` **and** `ENV_SDK_URL`, the switch's public
base/link URLs and the private Control Center's API/SDK coordinates. Inspect
the served `HyperLoader.js` to confirm both origins and test preflight/client
requests. Do not disable TLS, relax destination validation or allow Fake-IP
addresses to make this pass. In this development environment, Cloudflare
tunnel TLS failed with both OS Fake-IP DNS and a real DoH-resolved edge address;
public checkout/callback acceptance therefore remains pending.

Create the oxsum merchant under owner authority using `/business/merchants`:
rail `hyperswitch`, mode `test`, account ID from `merchant.json`, explicit local
API URL and exact-IP `internal` destination policy, private API/webhook keys,
publishable key and the reachable HTTPS SDK origin. A save is only local
validation, not payment certification. The current baseline customer order
path still selects the environment bootstrap merchant; owner-configured
checkout selection is tracked separately under N06 and must be completed.

Use the returned oxsum merchant/version IDs for the callback:

```bash
python3 infra/hyperswitch/public_proxy.py callback --port 18190 \
  --target-port <isolated-oxsum-port> \
  --callback-path /api/v1/webhooks/merchants/<merchant-uuid>/versions/<version-uuid>
# Tunnel only this proxy, then configure its exact public URL:
python3 infra/hyperswitch/merchant.py callback --url \
  https://callback-fixture.example/api/v1/webhooks/merchants/<merchant-uuid>/versions/<version-uuid>
```

Profile callback configuration enables signed payment/refund events. Actual
switch delivery is a separate check from the local replay in `smoke.py`.
Hyperswitch validates callback destinations itself; the script does not bypass
its DNS/IP policy.

## Real Stripe test connector

No PSP credential is supplied by this repository. Once a real Stripe test
account is available, an authorized operator supplies its `sk_test_...` key
privately to a **new** Hyperswitch connector with `connector_name: stripe`,
`connector_type: payment_processor`, the recorded profile, `test_mode: true`,
and Stripe's documented authentication fields. Configure its Stripe webhook
endpoint/signing secret through that account, select its routing explicitly,
and perform the genuine checkout/capture/refund/event/reconciliation matrix.
Keep dummy and real PSP records separately labeled. A Hyperswitch `snd_...`
key is not a Stripe credential. Never use live cards or a live key here.

## Stop, recovery and deliberate cleanup

```bash
python3 infra/hyperswitch/stack.py stop
python3 infra/hyperswitch/stack.py up
python3 infra/hyperswitch/merchant.py inspect
```

After recreation, the inspect command verifies merchant/profile/connector/key
identity and the signing/publishable values without printing secrets. Back up
the private runtime directory and this project's named volumes together.
Deleting `.runtime/private.json` while retaining volumes loses the encryption
identity; generating replacements is not a recovery strategy.

There is intentionally no automatic `down --volumes`, global prune, database
sweep or source reset. When this fixture is no longer needed, inspect the
recorded project and its Compose labels, stop it, then explicitly remove only
that project's containers and named volumes. Preserve other tasks' resources.
