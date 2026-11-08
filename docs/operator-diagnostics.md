# Operator diagnostics and recovery

`scripts/operator-diagnostics.py` produces a fixed JSON support report from the
running deployment. It uses Python's standard library and sends four reads to
the explicitly supplied origin: `/healthz`, `/readyz`, `/api/v1/version`, and
optionally `/api/v1/admin/diagnostics`. Only the last request carries the operator
token. Personal/business sessions, consumption keys and integration credentials
cannot read the aggregate endpoint. No operator token means the endpoint is
closed, even when the rest of the application is available.

```bash
python3 scripts/operator-diagnostics.py --url http://127.0.0.1:3000 \
  --token-file /private/operator-token --env-file deploy/.env \
  --output /private/diagnostics.json
```

Use a private regular file containing only the token; both input files must have
no group/other permissions, for example `0600`. `--token-file` and `--env-file`
are optional. The output is a new `0600` file. The script never sources an env
file, expands variables, runs shell substitutions or automatically reads `.env`.
It ignores unknown variable names and retains only `absent`, `partial` or
`present` for fixed documented groups. Values that reference another variable
count as text present, not validated effective values. Database presence accepts
`DATABASE_URL` or the Compose `POSTGRES_PASSWORD`; this does not validate either.
The runtime endpoint's flags describe the parsed running configuration, which
can differ from a local env file. Database-owned merchant accounts are counted
separately from environment payment adapters.

Targets must be explicit HTTP(S) origins without paths, userinfo, query or
fragment. Plain HTTP is accepted only for numeric loopback addresses; use HTTPS
for remote targets. Redirects and ambient proxy variables are ignored. Each
request has an eight-second timeout and a 32-KiB response cap. The report stores
neither target addresses nor input paths. Unexpected fields, types, envelopes or
version formats cause `invalidResponse`; HTTP failures retain only the status
code, and network failures retain only `unavailable`. No response error body is
copied. A report can be collected while the service is unavailable; exit success
means the report was written, so inspect the recorded states.

The runtime aggregate is a consistent read-only database snapshot with a
five-second statement timeout and a six-second total request deadline, including
pool acquisition. Schema comparison precedes aggregate table queries. Counts
are `null` on an unavailable, unmigrated or incompatible database; missing data
is never reported as zero. The three public probes and protected snapshot are
separate reads and may straddle a restart; compare their identities when
diagnosing an upgrade. A package/source/schema identity is not artifact signature
or candidate provenance verification.

## Reading the report

| Fields | Meaning and next action |
| --- | --- |
| `liveness`, `readiness`, `version` | Process response, exact schema readiness and build identity. A running process with failed readiness needs local startup/migration/database inspection. Use the install and upgrade runbooks; diagnostics do not apply migrations. |
| `databaseState`, `appliedSchemaVersion` | `ready` permits counts. `schemaMismatch` retains only a syntactically valid migration name. `unmigrated` or `unavailable` has no applied version. Do not downgrade a binary across an incompatible schema. |
| `configuration` | SMTP, public URL, credential sealing, head signing and payment adapter presence. Presence does not establish mail delivery, key decryptability, provider reachability, PSP acceptance or trust-key continuity. |
| `providerChannels`, credential active/cooling/disabled counts | Configuration and cooldown state only. Review the provider console privately if all credentials are disabled/cooling. Diagnostics make no provider probe or paid inference call. |
| `merchantsEnabled`, `merchantsDisabled` | Versioned database merchant account inventory. Enabled does not prove a valid credential or a certified payment path. |
| `jobsScheduled`, `jobsDue`, `jobsRunning`, `jobClaimsExpired`, `jobsDead` | Future runs, due runs, all active claims, claims older than the five-minute lease (or missing a stamp), and retained dead history. Due/running counts are normal transient states; compare repeated reports and inspect protected job details before intervening. A dead history row can remain after later successful runs. |
| `operationsOpen`, `operationLeasesExpired` | All created/leased/running operations, then the gateway sweeper's candidates: non-metering leased/running rows with expired leases or created rows older than the gateway lease (currently 60 seconds). Fresh admissions and metering are excluded from this recovery count; metering follows hold recovery. Let the configured workers act; do not invent a second request/payment identity to force completion. |
| `holdsOpen`, `holdsDead` | Hold watch rows and the subset marked for slower retry/attention. These are queue rows, not a scan or proof of ledger balances. |
| `paidOrdersUnfulfilled`, `paymentOrdersNeedAttention` | Confirmed payments waiting for their grant and orders fenced for review. The paid-order worker normally retries every five seconds. Check a persistent backlog and merchant event evidence; do not grant funds manually to hide it. |
| `merchantEventsPending`, `merchantEventsAccepted` | Event inbox classification/application backlog. No payloads or provider IDs enter the report. |
| `refundsRetryDue`, `refundsNeedReview` | Provider-pending order refunds with a due retry or no next retry. Future retries and terminal history are excluded. Review state/evidence before a provider action; an unknown result is not a successful cash refund. |
| `legacyRefundsRetryDue`, `legacyRefundsNeedReview` | Legacy pending/unknown deposit refunds with a due attempt or no next attempt. Retained uncertain historical refunds require explicit reconciliation. |

The report does not run reconciliation, retry queues, resend email, make external
calls, inspect ledger amounts or modify records. It does not collect full logs,
customer IDs, provider names/URLs, encrypted or plaintext secrets, payloads,
signing keys or error text. Review even this sanitized report before sharing its
operational counts. Keep detailed evidence private and follow [SUPPORT](../SUPPORT.md).

## Startup and recovery limits

When the application cannot start, the HTTP report records unavailability and
optional local presence groups. Run `scripts/check-config.sh deploy/.env`,
`scripts/check-secrets.sh deploy/.env`, and the image's `--check-config` locally
as described in the [installation runbook](self-hosted-install.md). Their output
and local startup logs require a separate review before sharing; the collector
does not execute or copy them.

For caller-visible overload, retained reservations, worker retries, refund
uncertainty or incompatible schemas, preserve the request's idempotency identity
and inspect the corresponding protected operational page/API. Counts alone do
not authorize a financial correction or prove conservation. Follow the
[upgrade/restore runbook](self-hosted-upgrade.md) and
[digest deployment procedure](self-hosted-release.md) for backups and compatible
rollback. This diagnostic feature does not establish candidate upgrade, active
operation drain, restore timing, old-proof verification or a post-restore new
call. Those remain N17/J16 acceptance work.
