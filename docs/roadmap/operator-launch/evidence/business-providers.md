# Owner provider connection editor evidence

Date: 2026-11-08. Issue #754, child of N04/#730. Development evidence only;
this does not close J03 or final candidate acceptance.

## Implemented scope

The business channel list now supports owner connection creation/replacement,
connection editing, exact internal destination grants, write-only credential
addition, status/removal, billing credential set/clear, and explicit audited
HEAD checks. Generated types describe the existing destination-policy read.
The channel navigation requires the same catalog capability as its read API;
secret writes and HEAD checks require the stronger owner capability.

A dialog preserves its inputs and operation key after failure. Editing a field
changes the key; success/close discards secret inputs. No automatic mutation
retry is configured. The list refreshes after writes and detail reads the current
row by identity instead of retaining a stale row object. Cursor paging and
English/Chinese copy are included. Destructive actions name the target and
collect a reason in the shared AlertDialog primitive. Set-by-name creation
explicitly warns about replacing a pre-existing credential pool.

## Executed checks

- `npm --prefix web run codegen`: generated schema matches OpenAPI.
- `npm --prefix web run build`: TypeScript and production Vite build passed.
  Vite retains the existing large-chunk warning; this is not a performance pass.
- `scripts/fmt-check.sh`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo nextest run -p oxsum-server --test business_providers --test
  provider_destinations`: 2 passed, 0 skipped, PostgreSQL 17.
- `cargo nextest run -p oxsum-core --test business_providers`: 9 passed,
  0 skipped. Includes cross-business and role denial, audit atomicity, concurrent
  replay/removal, TOTP/ownership change, secret rotation and old serving snapshots.
- An actual isolated listener reused this task's owner fixture database. Current
  owner login/context, create/replay, internal policy readback, last-four-only
  response, last-active removal refusal, add/disable/remove rotation, billing
  set/clear, endpoint-change key requirement and automatic billing-key clearing
  all passed through HTTP. A loopback stub observed exactly one HEAD across an
  exact check replay and zero POST/inference calls; saves sent no upstream call.
  Sanitized output: task-local `provider-ui/listener-evidence.json`.

## Unperformed acceptance

The prior ego-browser task space 4 no longer exists; the browser inventory only
contains unrelated user-owned spaces. No replacement space was created and no
unrelated space was claimed. Desktop/mobile rendering, keyboard focus,
English/Chinese workflows and browser network-retry behavior remain pending.
They must be exercised before marking J03 or the integrated candidate accepted.
API evidence is not a substitute for those browser checks.

Model/alias/cache/fallback editing is the next N04 child. No real provider call,
credential authentication, external payment or provider certification occurred;
the shared DeepSeek budget remains 0/5 attempts and CNY 0.
