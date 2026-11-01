# Implementation spec — issue #505: request analytics

Goal: a per-request analytics surface. Today we cannot answer "show me every
request this key made to gpt-4o-mini yesterday 14:00–15:00 with token counts
and latency". This issue adds an `oxsum.request_log` table, a query endpoint,
and the relay instrumentation that fills it — all post-settlement and
non-blocking so a write failure never fails a request.

Work only inside this worktree (`feat/request-analytics`). One commit per
logical step: migration, contract, core write + relay instrumentation, route +
frontend, tests, docs. Do not touch main; do not open the PR — the parent does
that after reviewing the diff.

## 1. Migration `crates/core/migrations/0068_request_log.sql`

Follow the existing migration comment style (a header comment block naming the
issue and the why, matching `0067_model_fallbacks.sql`).

```sql
-- Request analytics (issue #505): one row per admitted gateway request, the
-- operational record a billing dispute or an SRE query actually reads. The row
-- writes *after* the turn settles, off the settle path's success — a logging
-- failure is logged, never a failed request.
CREATE TABLE IF NOT EXISTS oxsum.request_log (
    request_id          text        NOT NULL,
    tenant_id           text        NOT NULL,
    organization_id     uuid        NOT NULL,
    key_id              uuid,
    model_requested     text        NOT NULL,
    model_served        text        NOT NULL,
    channel             text        NOT NULL,
    admission_outcome   text        NOT NULL,  -- 'admitted'|'refused' (a refused turn still logs, charge 0)
    ttft_ms             integer,               -- ms to first upstream byte; NULL on non-streamed/refused
    total_ms            integer,               -- ms admission->settle
    input_tokens        integer     NOT NULL DEFAULT 0,
    output_tokens       integer     NOT NULL DEFAULT 0,
    cached_tokens       integer     NOT NULL DEFAULT 0,
    charge_minor        bigint      NOT NULL DEFAULT 0,
    upstream_attempts   integer     NOT NULL DEFAULT 1,
    status              text        NOT NULL,  -- settled|errored|refused (mirrors usage_records.status)
    error_kind          text,                  -- settlement kind for non-settled; failure_reason text
    http_status         integer,               -- the gateway's own answer status, when it served one
    created_at          timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (request_id, tenant_id)
);
CREATE INDEX IF NOT EXISTS request_log_key_time   ON oxsum.request_log (tenant_id, key_id, created_at DESC);
CREATE INDEX IF NOT EXISTS request_log_model_time ON oxsum.request_log (tenant_id, model_served, created_at DESC);
CREATE INDEX IF NOT EXISTS request_log_time       ON oxsum.request_log (tenant_id, created_at DESC);
```

Keep `organization_id` (the org uuid) beside `tenant_id` (the ledger tenant)
because the endpoint is org-scoped for members — they filter by their org, not
a raw tenant. `admission_outcome`/`error_kind`/`http_status` let the analytics
answer "which requests failed and why" without joining the ledger.

## 2. Contract `crates/server/openapi.yaml` — bump to `0.93.0`

Add `GET /api/v1/requests` (session or admin-scoped read):

- Query params (all optional, camelCase): `keyId`, `model`, `channel`,
  `from`, `to` (RFC3339), `outcome` (`admitted|refused` — or reuse the
  `settled|errored|refused` status set; pick `status` to mirror
  `usage_records`), `minCharge` (minor units int), `limit` (default 50, cap 200),
  `cursor` (keyset pagination on `(created_at, request_id)`).
- Response: `{data: {rows: RequestLogRow[], nextCursor?: string}}` — follow the
  existing `data`-envelope and cursor conventions used by `/billing-records`.
- `RequestLogRow` schema mirrors the columns above (camelCase).
- Document org-scoping: a member sees only their org's rows; the admin variant
  `GET /api/v1/admin/requests` sees all orgs and takes an `organizationId`
  filter. Register both paths.
- Also add `requestLogRetentionDays` (integer, nullable) to the admin
  organization PATCH body if one exists for per-org retention override — or, if
  simpler, keep retention global via `OXSUM_RETENTION_REQUEST_LOG_DAYS`
  (default 30). Pick whichever is the smaller contract change; a global env var
  is acceptable for v1.

## 3. Core write — `crates/core/src/usage.rs` or a new `request_log.rs`

- `RequestLogRow` struct mirroring the table.
- `Db::record_request_log(&self, row: &RequestLogRow)` — a plain
  `INSERT ... ON CONFLICT (request_id, tenant_id) DO NOTHING`. It must NOT run
  inside `record_usage_in`'s transaction: it is best-effort, called after the
  settle commits, and a failure is logged via `tracing::warn!` and swallowed
  (return `Result` but the relay `.await`s it in a context that drops the
  error to a warn).
- `Db::request_log_page(&self, org_id: Option<Uuid>, filters..., cursor, limit)`
  — keyset paginated read. When `org_id` is `Some` scope to that org; `None`
  is the admin all-orgs read.
- `Db::sweep_request_log(&self, older_than_days: i64) -> rows_deleted` — for the
  retention job; reuse the existing retention pass if it has a table sweep
  helper (`run_retention` in `crates/core/src/`), else wire into it.

## 4. Relay instrumentation — `crates/server/src/gateway/relay.rs`

- `Plan` already carries `started: Instant`, `requested_model`, `model`
  (served), `channel`, `price_version`, `upstream_attempts`, `key_id`,
  `organization_id`, `tenant_id`, `request`, `status`/kind at settle.
- Track `ttft`: add an `Option<Instant>` set on the first upstream byte/chunk
  observed (in `observe` for streamed, or where the first body byte lands for
  non-streamed). `ttft_ms = first_byte - started` when present.
- After `plan.write` commits the settlement (the place `record_usage` was
  called), build a `RequestLogRow` from the plan and call
  `record_request_log`. `total_ms = started.elapsed()`. Do it in the same
  spawned settle task but AFTER the commit, and swallow errors to `warn!` —
  never propagate.
- Also log a row for admission *refusals* the relay already records (`refused`
  usage rows in #233) so the analytics surface can answer "which requests were
  refused" — `admission_outcome='refused'`, `charge_minor=0`, no ttft.

## 5. Routes — `crates/server/src/routes/`

- `usage.rs` (or a new `requests.rs`): `GET /api/v1/requests` — session-authed,
  org-scoped to the principal's org, keyset paginated. Parse the query into the
  filters, call `request_log_page`, answer the `data` envelope + `nextCursor`.
- Admin twin in `crates/server/src/admin/` — `GET /api/v1/admin/requests`,
  operator-token gated, `organizationId` filter, all orgs when absent.
- Wire both into `crates/server/src/routes/mod.rs` (and the admin router).

## 6. Frontend — `web/`

- `npm --prefix web run codegen` to regenerate `schema.d.ts` (it is tracked).
- Requests page (`web/src/pages/Requests.tsx` or wherever the requests list
  lives): add a filter form (key, model, channel, from/to, outcome/status,
  minCharge) and a table that pages via `nextCursor`. A row links through to
  the settlement entry / proof bundle the way the existing requests page does.
- Keep it consistent with the existing page's component vocabulary (`Card`,
  `Field`, `TextInput`, `Button`, table primitives from `components/ui`).

## 7. Retention

- Reuse `OXSUM_RETENTION_REQUEST_LOG_DAYS` (default 30) or fold into the
  existing `run_retention` pass — sweep `request_log` rows older than the
  window inside the same periodic job (`crates/server/src/jobs.rs` RETENTION
  kind). `.env.example` + `docs/development.md` gain the variable.

## 8. Tests — `crates/server/tests/requests_log.rs` (new)

- A settled turn writes a `request_log` row with the served model, channel,
  tokens, charge — read back through `GET /api/v1/requests`.
- A refused admission logs a row with `admission_outcome='refused'`,
  `charge_minor=0`.
- Filters narrow correctly (key, model, from/to, minCharge).
- A member sees only their org's rows; the admin endpoint sees all and honors
  `organizationId`.
- Cursor pagination walks the full set without repeats.
- A core unit test can pin `sweep_request_log` if you add it.

## 9. Docs (same commits)

- `docs/product.md` — the analytics surface.
- `docs/api.md` — the endpoint.
- `docs/architecture.md` — where `request_log` sits (post-settlement
  projection, not a billing fact).
- `docs/decisions.md` — append an entry: Postgres for v1, ClickHouse deferred;
  non-blocking post-settle write.
- `docs/user-guide.md` — the requests-page filter.
- `docs/development.md` + `.env.example` — the retention env var.
- `TODO.md` — mark issue #505 in the appropriate section.

## Verify before returning

- `cargo fmt --all`, `cargo clippy --workspace --all-targets` clean.
- `cargo nextest run -p oxsum-server --test requests_log` passes.
- `npm --prefix web run build` (tsc+vite) clean.
- Report: files changed, the contract version bump, test names added, and any
  deviation from this spec with the reason.
