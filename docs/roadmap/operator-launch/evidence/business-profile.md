# Persistent business profile development evidence

Date: 2026-11-08. Child [#756](https://github.com/AllureCurtain/oxsum/issues/756),
N03/#729, R02/J02. Base: `0314cbd3d5439adbc904a327a8e16704b344a0af`.
Environment: local PostgreSQL 17 scratch databases, Rust 1.99.1, Node 24.22.1;
no provider request, payment or production mutation.

## Implemented behavior

The business row stores its public name, contact email, help/logo URLs and
accent. Before the first save, environment branding supplies defaults and an
initialized business keeps its accepted setup name. A complete explicit save
pins all fields, including nulls, against subsequent environment changes.
Historical billing documents and financial settings are unchanged.

`GET /business/profile` checks current overview authority. `POST` requires the
owner's `profileWrite`, business context and applicable second factor, plus a
reason, expected revision and idempotency key. Profile, audit and immutable
mutation snapshot commit together. An unchanged retry returns its original
snapshot after later edits; changed requests and stale revisions conflict.
Public `/branding` resolves the same stored identity and does not mask database
failures as a successful defaults response.

The owner form retains its draft and operation key after a failed response;
editing changes the key. Discard reloads the authoritative revision. Both
locales and shared login/customer/business contact links are implemented.

## Executed checks

- `npm run codegen` and `npm run build`: passed.
- `scripts/fmt-check.sh`: passed after final source/test changes.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- Focused core profile/staff tests (4) and server branding test (1): passed.
  They cover unsafe links/email/CSS, required nullable replacement fields,
  first-save/default precedence, explicit clear, public reads, role/context/
  machine denial, stale authority, exact/changed replay, concurrent revisions
  and injected audit failure with no partial update.
- `cargo nextest run --workspace --no-fail-fast`: **1,810 passed**, six ignored,
  229.785 seconds executing tests, with real isolated PostgreSQL access.
  The first run caught a missing table in the migration inventory assertion;
  the expected `business_profile_revisions` table was added, preserving the
  exact inventory check, then the full workspace was rerun successfully.
- Ignored checks are two real-provider tests, two fixture-regeneration helpers,
  the gateway load check and the sustained capacity test. They are not counted
  as passes. This development run does not replace final full candidate CI or
  the separately required process-shared/capacity/provider acceptance.

Private local logs are retained at
`local-notes/operator-launch/profile-workspace-nextest.log` (initial failure)
and `profile-workspace-nextest-final.log` (complete pass). They are not release
artifacts and contain no real-provider credentials.

## Remaining acceptance

The original task-owned browser space disappeared before this child. No other
user space was claimed and no replacement was created. Desktop/mobile,
keyboard/focus, both locales, reload and failed-draft browser checks remain
pending; build/source review is not browser evidence. Required CI and actual
review must pass before normal integration. N03's settlement-currency and
funded-data guard remain a separate child. No final candidate verdict is made.
