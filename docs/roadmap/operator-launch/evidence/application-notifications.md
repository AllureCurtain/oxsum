# Application lifecycle notification development evidence (#937)

Date: 2026-11-10, Asia/Shanghai. Parent #747 / #725, N21 / J18.
Source: `feat/application-notifications`, based on PR #936 (`06359d9e`).
Environment: local PostgreSQL 17, isolated scratch databases and owned loopback
receivers; Python SQLite is a separate application inbox. No real PSP/provider
call, external receiver, public publication or independent pilot is claimed.

## Behavior and boundaries

Migration 0128 records owner-authorized immutable destination/policy/sealed-key
identity, exact original signed bytes and durable delivery claims. Creation
reveals the signing secret once; exact replay never reveals it. Current owner
IntegrationsManage gates create, disable and failed-cycle retry. Application-only
notificationsRead grants safe metadata without inheriting customer webhooks or
signing material. Two enabled/twenty retained endpoints bound per-application
configuration, and each source transaction fans out to at most two targets.

Source-table triggers enqueue customer/key/plan/period/order/refund/dispute/
statement/evidence and usage creation/status identity signals inside the source
transaction. Portal order changes disclose only the application's customer
identity, not the foreign order identity. The payload contains no financial state,
supplier cost, customer key or merchant secret. Replays and stale signals request
current scoped reads; timestamps do not determine financial order. There is no
historical backfill, natural-time-expiration guarantee or arbitrary ledger CDC.

Delivery claims at most 32 rows with eight concurrent sends, existing ten-attempt
backoff and a 120-second claim fence. Fixed sanitized error categories avoid
receiver content and secrets. ProviderHttp pins validated DNS, disables redirects
and proxies, defaults to public HTTPS and respects exact owner-approved internal
IP policy. Disable stops new/unclaimed work; already-claimed attempts may finish.
Credential revocation stops API reads separately from owner-authorized delivery.

The independent Python receiver verifies HMAC over original bytes before parsing,
checks application/header/body identity, commits its SQLite inbox before 204,
deduplicates exact delivery IDs and keeps failed reconciliation pending. It reads
current facts rather than projecting state from old events. Bounded billing/key
reads are explicitly not a complete statement export. Complete application
adoption and scoped statement detail remain separate tasks.

## Executed validation

- `cargo nextest run --workspace --no-fail-fast`: **2,194 passed**, 6 existing
  opt-ins skipped, 346.458 seconds test execution. Scratch database suites ran;
  this is local development evidence, not executed candidate full CI.
- Before full regression, 25 focused core/HTTP/existing-webhook checks passed.
  Actual paid plan, order, refund, statement and cancellation writers enqueue
  committed identities. Authority/replay/rotation/rollback/secret/lease/retry/
  disable boundaries pass. Rust HTTP sends real original-byte signatures and
  passes captured output over stdin to Python for independent verification and
  durable duplicate intake.
- Python receiver: **3 passed**; frontend grant/link behavior: **10 passed**.
- Four OpenAPI authority contract tests, generated types and production frontend
  build passed. Existing >500 kB chunk warning remains.
- Final core/HTTP/application/webhook/identity selection: **38 passed**, zero
  skipped, 6.887 seconds. Includes bounded 65-signal pagination, unchanged-write
  suppression and disjoint concurrent 32-row claims; the actual application sender
  rejects a public receiver resolving to loopback before any send. These two added
  cases bring the ordinary full/focused passing inventory to **2,196**.
- Strict core/server all-target Clippy passed in 3m19s. Pinned formatting and diff
  checks passed. No production Rust changed after the full run; final test/UI-only
  fixes use the focused evidence and rebuilt frontend.

The owner UI includes both locales, endpoint inventory, one-time secret copy,
failed-delivery recovery, exact frozen retries/new intent, pagination and explicit
stop confirmation naming the destination. Skeleton/empty/error states compile;
this is not observed desktop/mobile/keyboard browser acceptance.

## Remaining acceptance

Independent review, protected integration, changed-flow browser acceptance and
candidate J18 remain. Named statement checkout #935 is a sibling using the same
existing order/statement source writers; join both stacks for final application
acceptance. A separate-store complete application, independently retained trust
and actual candidate listener are still required. No release readiness follows
from this development record alone.
