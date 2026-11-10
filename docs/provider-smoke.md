# Bounded provider exercise

`scripts/bounded-provider-smoke.py` runs two fixed OpenAI-compatible chat requests
against an explicitly selected oxsum artifact: whole response and SSE. It creates
and drops its own random local PostgreSQL database. Current owner consent,
customer invitation, synthetic receipt replay and explicit sale terms establish
the fixture; synthetic funding is not a PSP payment.

The gateway routes to an explicitly approved loopback guard. That guard alone
holds the external credential and forwards only the exact bounded request to
`api.deepseek.com:443`, with public-address validation, certificate verification
and the original TLS hostname. It resolves the hostname through authenticated
Google DNS-over-HTTPS and pins a validated IPv4 answer. It rejects Fake-IP,
private and reserved addresses. It neither follows redirects nor retries an
external request. This mediated path is recorded in evidence; it does not claim
to certify the ordinary production resolver or every provider protocol.

## Artifacts and local rehearsal

Install `psycopg[binary]` in a task-owned Python environment and have Node/npm
available. Supply actual existing native binary and verifier tarball identities;
the runner freezes and rechecks the bytes before use. It does not build them.
Only a local PostgreSQL provisioning URL with CREATEDB authority is accepted;
its ordinary database is never selected as the server database.

```sh
python3 scripts/test-provider-smoke.py
python3 scripts/bounded-provider-smoke.py \
  --binary /absolute/oxsum --sha <40-hex-source-sha> \
  --sha256 <64-hex-binary-digest> --schema <reported-schema-version> \
  --sdk /absolute/verify-oxsum-bill.tgz --sdk-sha256 <64-hex-package-digest> \
  --database-url postgres://postgres:postgres@127.0.0.1:5432/postgres \
  --output /absolute/new-fixture-evidence \
  --budget /absolute/new-fixture-budget.json --fixture
```

Fixture mode creates a fresh separate test journal and never reads a provider
credential or sends an inference. It refuses the real run-wide journal path.
Its numerical usage/cost values are synthetic and are labeled as such.

## Authorized live exercise

The operator-launch authority allows CNY 0.10 total and five upstream attempts
across all worktrees/resumes. The default exercise makes exactly two. The original
journal is `local-notes/operator-launch/provider-budget.json` at the primary
repository root; live mode refuses a different path and never creates/resets it.

Before a live run, verify current official pricing and record `pricing_verified_at`
(as a timezone-aware ISO timestamp within 24 hours), `input_bound_tokens: 4096`,
and this exact current pricing object in the existing journal while holding its
adjacent `.lock` file with an exclusive `flock`. Preserve every existing attempt,
scenario, reservation and total. This is an explicit audited preparation step;
the runner never assumes an old price is current.

```json
{
  "model": "deepseek-flash",
  "input_micros_per_token": 2,
  "output_micros_per_token": 8,
  "source": "https://api-docs.deepseek.com/zh-cn/quick_start/pricing"
}
```

The object belongs in the journal's `pricing` field. These are peak cache-miss
rates in CNY millionths per token, not customer sale prices. As verified on
2026-11-09, the documented model version is DeepSeek-V4.1-Flash-1017. The fixed
ASCII payload is under 512 bytes, contains only `Reply with exactly: OK`, and has
no tools, search or conversation history. The conservative 4,096-token bound
includes serialization/chat-template overhead. At most 32 output tokens and
explicit `thinking: {type: "disabled"}` produce a reservation of 8,448 CNY
millionths per request. All five possible lifetime reservations fit under CNY
0.10; no cache or off-peak discount is needed for the bound.

Run the same command without `--fixture`, with the original shared journal path
and `--credential-file /absolute/primary-repository/.env`. Only the
`OXSUM_E2E_UPSTREAM_API_KEY` field is read. The file must be private (no group/other
permissions); no other root setting is imported. Never put the key on the command
line or in a tracked file.

A scenario is durably claimed before gateway I/O. Each attempt takes a process
lock, writes a private temporary file, fsyncs it, atomically replaces the journal,
and fsyncs the parent directory before constructing the credential-bearing
transport. An already attempted scenario cannot send again, including gateway
or concurrent local repeats. Crashes/timeouts retain the full reservation.
Known usage adds a conservative peak-rate observed cost but does not restore the
lifetime reserved allowance. A usage-bound violation halts further attempts and
is retained in the sanitized report. Never delete/reset a claimed scenario to
retry automatically; recover its retained evidence or record the incomplete row.

## Evidence and remaining gates

The output records exact binary/SDK hashes, observed runtime identity, public
DNS observation, request IDs, usage, settled charge, balance/reservation checks,
price/sale proofs, retained operator key, signed head and consistency proof. Each
scenario's packed SDK check runs in a clean consumer after the listener stops and
the database is dropped. It checks the bill and rejects altered content, hash,
operator key and retained head. No provider key, customer session or private
runtime configuration is exported. The original private budget journal remains
the authority for attempts; sanitized attempt receipts are also included.

The explicit artifact may be a development build. This does not silently become
final-candidate acceptance. Other protocols, live client cancellation, missing
provider usage, external fallback and PSP certification require their own named
scenario/access evidence. A successful unauthenticated TLS HEAD proves network
reachability only, and uses no inference attempt.


## Executed development checkpoint (2026-11-10)

The [#944 development exercise](roadmap/operator-launch/evidence/deepseek-development.md)
passed whole/SSE against source `1e3dc087` and independently verified both exact
settlements offline. It consumed two of the original five attempts; the durable
journal retains CNY0.016896 reservations and CNY0.000052 conservative peak-rate
usage cost. Those completed scenarios are not repeatable automatically. This
checkpoint does not satisfy final-candidate or unexercised protocol/scenario rows.
