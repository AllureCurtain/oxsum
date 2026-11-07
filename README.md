# oxsum

[![CI](https://github.com/AllureCurtain/oxsum/actions/workflows/ci.yml/badge.svg)](https://github.com/AllureCurtain/oxsum/actions/workflows/ci.yml)

Oxsum is self-hosted billing for AI services, with evidence customers can keep and
verify. One Business owns its suppliers, sale terms and collection; personal and
team customer accounts own their balances, keys and bills. Operators use the
built-in portal at `/app/business`. AI application and Agent SaaS developers can
embed the gateway and scoped metering APIs in their own product. Oxsum takes no
platform fee in this deployment model.

Before an admitted model call, oxsum reserves the applicable upper bound, then
settles the recorded usage against the pinned terms and releases unused credit.
Purchased funds, promotional credit, plan quota and approved credit lines retain
their separate accounting rules. Unknown usage, estimation and capped settlement
are explicit outcomes. The Rust backend uses a double-entry ledger and an
append-only Merkle log; an independently retained hash, key and head let a customer
check the evidence within its [documented scope](docs/proofs.md). A proof does not
establish that the upstream token count was truthful or that every provider has
been certified.

The ledger engine is [doubleentry](https://github.com/hupe1980/doubleentry)
(MIT/Apache-2.0), vendored in `crates/doubleentry`. The React dashboard is embedded
in the Rust binary; PostgreSQL 17 holds the database.

## Start an operator deployment

Follow the [self-hosted installation runbook](docs/self-hosted-install.md) for the
recommended Compose installation, secrets, owner initialization and backups. For
a local development checkout:

```bash
git clone https://github.com/AllureCurtain/oxsum.git
cd oxsum
cp .env.example .env
# Set the database and required setup/sealing/signing values for this instance.
docker compose up -d
cargo run -p oxsum-server
```

Check [health](http://localhost:3000/healthz), then complete the runbook's one-time
owner setup and sign in at [the app](http://localhost:3000/app). The initialized
owner selects Business context to enter `/app/business`. Customer registration
is invite-only by default; `OXSUM_SIGNUP=open` does not override the persisted
business policy. Customer invitations and an explicit owner change to open
registration are the supported admission paths. Registration grants no purchased
funds or staff authority.

The current onboarding UI still has gaps tracked in the
[operator-launch work](https://github.com/AllureCurtain/oxsum/issues/725).
Use the documented setup APIs where the installed revision lacks a browser
editor. A configured channel or merchant is not evidence of a successful model
call or payment. Keep the deployment token in operator tooling; customer
browsers do not need it.

## Try and verify locally

The [maintained integration examples](docs/integrations.md) cover scoped
management, consumption and signed webhooks. The standalone
[`examples/verify-consumer`](examples/verify-consumer) checks an export using a
locally packed SDK outside the workspace, including tampered evidence rejection:

```bash
bash scripts/check-verify-consumer.sh
```

The old `demo/demo.py` flow predates current customer authority and safe database
isolation. Its replacement is tracked in
[#728](https://github.com/AllureCurtain/oxsum/issues/728) /
[PR #751](https://github.com/AllureCurtain/oxsum/pull/751); use its own revision's
instructions after integration. Do not run the old script against an ordinary
database or use its historical transcript as current acceptance evidence.

A customer funds an account through a verified purpose-specific order, an
applicable entitlement, redemption, or an authorized staff-recorded receipt.
`POST /api/v1/topups` no longer permits customer self-credit. Requests and Bills
expose settlement evidence immediately. Period reports require the covered UTC
days to close. `/verify` performs the browser check when the WASM assets are
installed and has a server form fallback; the packed SDK provides a separate
offline verification path.

Historical screenshots illustrate the earlier UI, not acceptance of the current
candidate:

Every tenant head is cosigned by an independent witness — a second oxsum process with its own key and database — so `/api/v1/log/checkpoint` serves a third-party `cosignature/v1` alongside the operator's signature, and the bill page reports "Witnessed by N." The same check is a product, not only a page: the [`verify-oxsum-bill`](web/verify-sdk) package verifies a bill in any browser, and its `<verify-bill>`, `<verify-statement>` and `<verify-usage-report>` elements drop the whole proof card onto a host page with one `<script>` tag. The package is **not yet published to npm** (`npm view verify-oxsum-bill` answers 404); build it from `web/verify-sdk`, or run the repository's [`examples/verify-consumer`](examples/verify-consumer) against a locally packed tarball — `bash scripts/check-verify-consumer.sh` is the reproducible path. The wire protocol both speak is [docs/transparency-log-v1.md](docs/transparency-log-v1.md).

## Demo

`demo/demo.py` runs an isolated local flow through the official OpenAI Python SDK: deployment-authorized owner consent, an invited customer, a synthetic manual receipt, explicit customer sale terms, one streamed stub response, and a verified settlement. It uses a unique disposable PostgreSQL database and **fails if it cannot create one**. It never uses the provisioning database for application data, reads no `.env`, and contacts no paid provider or PSP.

Prerequisites: Python 3.10+, Rust, Node 20.16+, npm, PostgreSQL 17 with a `CREATEDB` role, the `wasm32-unknown-unknown` Rust target and the matching `wasm-bindgen` CLI (see [development requirements](docs/development.md#requirements)). The command builds the server and verifier; Cargo's configured shared target directory is respected.

```bash
python3 -m venv /tmp/oxsum-demo-venv
/tmp/oxsum-demo-venv/bin/pip install -r demo/requirements.txt
docker compose up -d            # local PostgreSQL, if needed
/tmp/oxsum-demo-venv/bin/python demo/demo.py
```

The provisioning connection defaults to `postgres://postgres:postgres@127.0.0.1:5432/postgres`. Set `DATABASE_URL` in the process for another local cluster; it is used only to create/drop this run's random database. The application receives only its new database URL and generated keys, with all inherited `OXSUM_*` configuration removed. No database privilege failure permits a fallback or a channel-table reset.

The deterministic turn reports 23 input and 11 output tokens at one minor unit per token: the charge is exactly **34**, the available balance is **99,999,966**, and no reservation remains. The retained hash, pre-turn head/key, signed supply/sale bindings, consistency proof and new head go into the printed evidence directory (`--output <new-directory>` chooses it). The key is retained from this local fixture; it is not an independently attested operator identity.

After stopping the server and dropping its own database, the script installs the locally packed `verify-oxsum-bill` tarball in a clean directory outside the repository and runs the maintained [offline consumer](examples/verify-consumer). Bill, signature and history checks must pass; altered content/hash/key/archived-head checks must fail. A required failure exits nonzero. The tarball, checksums, sanitized evidence and verification log remain available. This proves an immediate entry, not a closed-period report, independent witness, real payment or real-provider certification.

The dashboard surfaces the same flow: a top-up on **Top up** (manual, Stripe, Airwallex, Hyperswitch or a redeem code — the Hyperswitch tab mounts the switch's Unified Checkout in place), billed turns on **Requests** and aggregated on **Usage**, the ledger entries with their proof bundles on **Bills**, and the verification itself on **`/verify`**:

| | |
| Funding | Evidence |
| --- | --- |
| ![Historical Hyperswitch top-up screen](docs/assets/topup-hyperswitch.png) | ![Historical Bills screen](docs/assets/bills.png) |

## Build a local image

```bash
docker build -t oxsum .
```

The image contains the binary, React app and verifier assets; it requires an
external PostgreSQL and deployment configuration. Use
[`deploy/docker-compose.core.yml`](deploy/docker-compose.core.yml) and the
[installation runbook](docs/self-hosted-install.md) rather than treating a healthy
process as a fully initialized business. `.env` is excluded from the image.

## Evidence and release status

The earlier self-hosted program's
[acceptance results](docs/roadmap/self-hosted-v1/ACCEPTANCE-RESULTS.md) and
[release checklist](docs/roadmap/self-hosted-v1/RELEASE-CHECKLIST.md) are historical,
SHA-specific evidence. The next operator-launch program is
[#725](https://github.com/AllureCurtain/oxsum/issues/725); it includes portal setup,
embedded application lifecycle, daily revenue recognition and exact-artifact
acceptance. A green fast PR run is not a full candidate pass.

Provider sandbox certification, required reference capacity, independent pilots
and publication remain distinct gates. The [rail matrix](docs/rails.md) separates
local fixtures from actual provider and live-money evidence. No production-ready
label or npm publication is inferred here. Build `verify-oxsum-bill` from
[`web/verify-sdk`](web/verify-sdk) or use an explicitly supplied tarball.

The historical public demo at [oxsum.785777.xyz](https://oxsum.785777.xyz) is a
separate deployment. Its availability, credentials, balances, provider access and
current build have not been revalidated by this documentation update. A second
local witness process demonstrates the protocol; organizational independence
requires a separately trusted operator and key.

## Documentation

- [Product overview](docs/product-page.md), [behavior](docs/product.md) and [user guide](docs/user-guide.md)
- [API](docs/api.md), [integrations](docs/integrations.md) and [unsupported operations](docs/unsupported.md)
- [Verify a bill](docs/verify-your-bill.md), [proof scope](docs/proofs.md) and [payment evidence](docs/rails.md)
- [Install](docs/self-hosted-install.md), [upgrade/restore](docs/self-hosted-upgrade.md) and [candidate/release gates](docs/self-hosted-release.md)
- [Development](docs/development.md), [architecture](docs/architecture.md) and [decisions](docs/decisions.md)
- [Security](SECURITY.md) and [support](SUPPORT.md)

## License

MIT OR Apache-2.0. `crates/doubleentry` retains the original author's copyright
notices; see the LICENSE files in that directory.
