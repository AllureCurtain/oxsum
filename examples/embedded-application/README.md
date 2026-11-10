# External AI application with its own store

This small Python application owns a private SQLite store and runs separately
from oxsum. It maps a local signup to an application-scoped billing account,
creates a bounded customer key, starts server-quoted checkout, sends one bounded
whole-response AI request, retains the receipt, and exports verifiable evidence.
A separate notification process reconciles signed signals into the same local
store. Runtime never logs in with an owner cookie or password.

This is a local, single-operator example for Python 3.11+ on macOS/Linux (POSIX
file locks), Node 20.16+ and SQLite. It is not a public multiuser login service.
The command's `customer` is the local app's authenticated account selector; a
public application must supply that selector from its own authenticated session.
Do not let an untrusted caller choose another customer's local identity.

## One-time setup

An operator creates an application and grants only the operations this example
will use. Its full walkthrough uses customersProvision, customersRead,
customersLifecycleWrite, customerKeysWrite, entitlementsRead, checkoutWrite,
billingRead, evidenceRead, evidenceWrite and notificationsRead. The example does
not need meteringWrite or customerLinkWrite. Narrow deployments may omit commands
and their grants. Scope failure remains an error; the example never logs in as
owner to work around it. Existing portal-account enrollment uses the separate
verified consent flow; this example's signup uses service-managed accounts.

Configure a published model/sale and collection route in the operator portal.
To buy a plan, the operator first assigns its sold version. Funding is never
performed by the application credential. A pending checkout cannot admit paid
usage. Select a test merchant for development; the automated fixture uses only
local provider/payment stubs and a labeled verified-event fixture.

Keep runtime secrets out of argv and tracked files:

```sh
export OXSUM_APPLICATION_CREDENTIAL='<owner-issued oxs-int- credential>'
APP_ID='<owner-created application UUID>'
APP_DB='/absolute/private/path/application.sqlite'
BASE='http://127.0.0.1:3000'
app() {
  python3 examples/embedded-application/app.py \
    --database "$APP_DB" --application-id "$APP_ID" --base-url "$BASE"
}
```

The store is bound immutably to one origin and application UUID. Its files are
mode 0600; a newly created directory is 0700. The database contains customer
consumption secrets, checkout handles, prompts and responses. Protect the store
and its backups. The application management credential comes from the environment
and is not stored. Credential rotation preserves the store and original operations.
The example refuses redirects and environment proxies when attaching credentials.

## Signup, key and current facts

Commands are JSON on stdin. Keep each `operation` permanently associated with
one exact command/customer. Reusing it with different inputs refuses locally.
All amounts are integers; statement/debt/key limits use credit minor units
(1 credit = 1,000,000), while checkout `displayAmountMinor` is fiat minor units.

```sh
printf '%s\n' '{"action":"signup","customer":"alice","operation":"alice-signup","name":"Alice"}' | app
printf '%s\n' '{"action":"retrieve","customer":"alice"}' | app
printf '%s\n' '{"action":"read","customer":"alice","resource":"models"}' | app
```

Select a published model and a future expiry within the grant's expiry; replace
the illustrative values below. The example sets five requests/minute and one
concurrent hold, and prints only the key ID, never the consumption secret:

```sh
printf '%s\n' '{"action":"key","customer":"alice","operation":"alice-key","model":"YOUR_MODEL","spendLimitMinor":1000000,"expiresAt":"YOUR_FUTURE_RFC3339_TIME"}' | app
```

If the first secret-bearing response was lost, exact retry can recover metadata
but cannot reveal the secret again. Read `keys`, explicitly revoke the original
key using its returned ID and a new revocation operation, then deliberately issue
a replacement. The example never silently creates another key.

`read.resource` supports entitlement, models, keys, statements, orders, evidence
and billing. `statement` or `order` additionally needs `id`. Lists are bounded
windows, not a claim of complete history; preserve cursors or extend the consumer
for older pages before relying on it as a full accounting archive.

## Quote, payment and recovery

The following starts a one-currency-unit wallet checkout. Review your own
currency/rate before changing the amount:

```sh
printf '%s\n' '{"action":"checkout","customer":"alice","operation":"alice-wallet","selection":{"purpose":"walletTopup","rail":"hyperswitch","displayAmountMinor":100},"returnUrl":"http://127.0.0.1:8080/paid","cancelUrl":"http://127.0.0.1:8080/cancel"}' | app
```

The exact quote, customer, order and destinations are durable before order
creation. Retrying this command reuses its original quote/operation. For a
published bundle replace selection with `purpose: bundle`, rail and
`bundleVersionId`. For an assigned plan use `purpose: planPeriod`, rail and
`customerPlanId`. For a named issued statement use `purpose: statementRepayment`,
rail, `statementId` and optionally positive `statementAmountMinor`; omit the
amount for the currently available verified debt. Do not supply wallet fiat
amounts for plan/statement payment. These all use the server's existing quote.

A hosted rail returns its checkout URL. Hyperswitch uses the owner-configured SDK
and retained client handle; open the same order in the local private payment page:

```sh
python3 examples/embedded-application/checkout.py \
  --database "$APP_DB" --application-id "$APP_ID" --base-url "$BASE" \
  --customer alice --operation alice-wallet
```

Open the printed random-capability loopback URL. It exposes only that customer's
retained checkout and current status, never the application management credential.
The page loads the operator-selected HTTPS provider SDK. No browser return or SDK
message marks payment complete: refresh and read the actual order until its state
is `fulfilled`. `paid` before fulfillment is still an intermediate state. A local
fixture is not real PSP certification; real charges require a separate budget.

`read` with `resource: order` always fetches current state. A retained successful
checkout command returns its original response. If an earlier attempt is unknown,
retry its original command. After inspecting order history, an explicit `resume`
command takes customer, a new local `operation`, and `orderId`; the server recovers
the same immutable order/destinations. The consumer never treats retry as a new
payment, and has at most three durable network attempts per local operation.

## One AI turn and its receipt

```sh
printf '%s\n' '{"action":"ask","customer":"alice","operation":"alice-question","keyOperation":"alice-key","model":"YOUR_MODEL","prompt":"Reply with one short greeting."}' | app
```

The prompt is capped at 512 characters, output at 32 tokens and streaming is off.
The customer key enforces the finite budget. This makes an actual upstream call
outside the local fixture; use an explicitly authorized provider/budget. The
runtime stores body, customer/key identity, idempotency key and attempt count before
sending. A successful retry returns the saved answer; an unknown result retries
the same request at most three times and only within 23 hours of first intent.
The server's 24-hour idempotency retention is not an unlimited inference warranty.
After that window, stop and reconcile the receipt; never generate a new inference
just because the response is missing. Failed requests retain their original intent.

Obtain the exact organization UUID, ledger origin and operator public key through
a separate trusted channel. Retain them before proof download:

```sh
printf '%s\n' '{"action":"trust","customer":"alice","organizationId":"CUSTOMER_UUID","origin":"oxsum/ledgers/EXACT_TENANT","key":{"keyName":"OPERATOR_NAME","publicKey":"PUBLIC_KEY_HEX_OR_BASE64","keyHash":"8_HEX_DIGITS"}}' | app
printf '%s\n' '{"action":"receipt","customer":"alice","askOperation":"alice-question","directory":"/absolute/private/path/receipt"}' | app
```

Receipt export first retains the customer bill's entry ID/content hash separately
from the proof download. That hash is the server's earlier recorded commitment,
not a third-party attestation. Original proof/head JSON bytes are preserved without
rounding 64-bit amounts. If concurrent writes make proof and signed-head snapshots
differ, the read refuses; repeat the read without another inference. An old receipt
outside the bounded request/bill windows needs operator recovery.

## Evidence and offline verification

Discover customer statements, then publish one complete issued statement:

```sh
printf '%s\n' '{"action":"read","customer":"alice","resource":"statements"}' | app
printf '%s\n' '{"action":"publish","customer":"alice","operation":"alice-statement-export","scope":{"kind":"statement","statementId":"STATEMENT_UUID"}}' | app
printf '%s\n' '{"action":"evidence","customer":"alice","bundleId":"RETURNED_BUNDLE_UUID","directory":"/absolute/private/path/evidence"}' | app
```

For period publication use `scope: {kind: period, from: YYYY-MM-DD, to: YYYY-MM-DD}`
covering at most 31 ended UTC days. Today's live day is not a complete period.
Retained descriptor/digest and independent trust are saved before the original-byte
evidence download. Bounds/unsafe private captures fail explicitly. An export
reports `verified: false` until the independent verifier succeeds.

Install the identified candidate's verifier tarball in a clean consumer directory,
copy `package.json`, `verify.mjs`, `verify-negative.mjs` and `scope.mjs` there, then:

```sh
npm install --ignore-scripts --no-audit --no-fund /absolute/path/verify-oxsum-bill-VERSION.tgz
node verify.mjs /absolute/private/path/receipt
node verify.mjs /absolute/private/path/evidence
node verify-negative.mjs /absolute/private/path/receipt /absolute/private/path/evidence
```

Verification is offline and refuses network access. It binds retained hash/customer,
manifest, price/sale commitments, operator key and exact ledger origin. The negative
check rejects nine tamper/wrong-scope/missing-trust cases. These proofs do not certify
the provider's real work, economic fairness, external cash, witnesses or global
completeness beyond the documented scope. No private supplier cost is authorized.

## Notifications, archive and restored operation

The operator configures a notification endpoint once for this application. Run:

```sh
export OXSUM_NOTIFICATION_SECRET='<one-time whsec- secret>'
python3 examples/embedded-application/notifications.py \
  --database "$APP_DB" --application-id "$APP_ID" --base-url "$BASE" --port 8081
```

For the local example explicitly approve the exact loopback endpoint/IP using the
owner UI. Public use needs a production ingress; this receiver binds only loopback.
It writes the original signed bytes and delivery ID before acknowledging, then
fetches current scoped state into `snapshots`. A statement signal fetches its exact
complete statement. Duplicate/old events cannot restore archived state; failed
reads remain pending with bounded backoff; one unavailable statement or customer
does not block unrelated signals. Run one receiver worker per store. Initial/current-on-use
reads are still required.

```sh
printf '%s\n' '{"action":"archive","customer":"alice","operation":"alice-archive","reason":"Close this application account"}' | app
printf '%s\n' '{"action":"read","customer":"alice","resource":"entitlement"}' | app
```

Archive blocks new consumption while retaining authorized history. There is no
application unarchive command. Repeated reads recheck live credentials; cached
successful operation responses are historical local receipts, not fresh authority.
Revoking an application credential blocks new API access but does not itself stop
the owner's notification endpoint; disable the endpoint separately.

Keep the private SQLite file and `.lock` together. Stop all local processes before
copying/restoring a checkpointed database; this example does not implement online
backup. Reopen with the same deployment/application binding, then retry the exact
original operation. A restored older snapshot cannot extend server idempotency
retention: unresolved inference still has the 23-hour refusal.

## Executed development fixture

From the repository root:

```sh
scripts/check-embedded-application.sh
```

The script builds WASM, packs the verifier, installs it into a clean temporary
consumer, and runs Python checks plus the real-listener Rust fixture. The fixture
starts oxsum and owned model/merchant HTTP stubs, then invokes separate Python
processes against one app SQLite store. It tests signup/mapping, wrong customer/
namespace/scope, pending-payment denial, verified local event/fulfillment, lost
inference response/replay with one charge, original receipt and paid-plan statement
exports, actual packed-SDK offline verification/rejections, signed notification
intake/current archived state and revoked authority. No external model/PSP request,
public publication or real payment occurs. Development evidence and candidate,
browser and external certification verdicts remain separate.
