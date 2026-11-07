# Acceptance results — integrated closeout

Updated 2026-11-07. Issues [#621](https://github.com/AllureCurtain/oxsum/issues/621),
[#695](https://github.com/AllureCurtain/oxsum/issues/695). Contract:
[ACCEPTANCE.md](ACCEPTANCE.md).

The pinned code candidate is `813c788613f9f2a737562493c3b305ca1aaff5c1`
(PR #694 merge). This evidence update changes documentation only; later main
commits must not silently replace the pinned candidate. Full checks, image
identity and artifact links are recorded in
[evidence/closeout-candidate.md](evidence/closeout-candidate.md).

The previous blanket **A01–A30 pass is withdrawn**. Its candidate `930794c`
had failed full CI [42240404235](https://github.com/AllureCurtain/oxsum/actions/runs/42240404235)
even though candidate workflow 42240437323 was green. That old workflow did
not enforce full CI or boot the image. The original read-only soak provided
neither valid percentiles nor charged-call conservation, and its five browser
screenshots did not exercise payment/staff workflows.

Automated functional coverage, scoped browser results, capacity targets and
external certification are distinct. **D09 misses its performance target;
A31 remains external-pending.** The rows below describe the evidence's scope,
not an assertion that every supported deployment or external service is certified.

## Executed closeout checks

| Check | Environment and observed result |
| --- | --- |
| Relocated archive regression | Local PostgreSQL 17, original executable hidden: install/ownership tests **19/19**; full workspace then **1,779 passed**, five ignored; #686 |
| Payment recovery regression | Real scratch DBs: payment/refund/event/job selection **30/30**; full workspace selection excluding process-shared suites **1,780 passed**, six ignored; strict workspace Clippy; #693 |
| Browser | Built React UI and real server/DB with declared local model/payment stubs; scoped workflow/screenshots and exact accounting in [closeout-browser.md](evidence/closeout-browser.md); #691/#693/#694 |
| Capacity | Real TCP streaming, 1,000 billing accounts, 10,000 keys, 100 peak streams, five minutes; **5,897 successes, zero failures, zero drift**, performance target missed; [closeout-capacity.md](evidence/closeout-capacity.md); #689 |
| Exact code candidate | Full CI, independent packaged verifier and no-push image startup: [closeout-candidate.md](evidence/closeout-candidate.md) |

The first attempted payment regression while local PostgreSQL was stopped
returned early in the old DB harness. That run is discarded; only the rerun
with actual scratch-database creation counts. Ignored fixture-generation and
opt-in load tests are not passes. Final CI separately executes the three
integration shards, process-shared suites and engine Postgres conformance.

## Scenario matrix results

| ID | Verdict | Evidence |
| --- | --- | --- |
| A01 Capability matrix | pass | `crates/server/tests/faults.rs::every_mounted_route_is_classified` + `scripts/check-route-authority.sh` (all 81 authenticated routes classified, none stale); per-role denial matrix in `financial_authority.rs`, `consumption_scope.rs`, `business_console.rs::a_support_cannot_void_a_journal`; denied writes assert unchanged balance/entries |
| A02 Forged funding/usage | pass | `financial_authority.rs` (customer self top-up/adjust/meter/refund-confirm denied, wallet unchanged); `payment_orders`/`merchant_events` suites — checkout mints `pending` only |
| A03 Migration | pass | `install_ops.rs::previous_version_fixture_upgrades_intact`, `migrate_only_brings_a_fresh_database_current`; T02 evidence `evidence/T02.md` — legacy IDs/balances/roots/keys unchanged, rerun idempotent, no auto-promotion |
| A04 Setup and membership | pass | `business_setup.rs`, `business_setup_fresh.rs`, `business_staff.rs`, `invitation_consent.rs`, `invitations.rs` — setup races/expiry, invite-only default, revoked staff, last-owner transfer, context switch |
| A05 Identity stability | pass | `customer_lifecycle.rs`, `suspension.rs` — rename/archive preserve history and in-flight operations; manual vs dunning suspension distinct |
| A06 Isolation | pass | `faults.rs` (tenants share, cache bound, cross-tenant probes), `consumption_scope.rs` (14 Personal/AccountManage/Retired probes 403), `groups.rs` |
| A07 Provider setup | pass | `business_providers.rs`, `provider_destinations.rs`, `merchants.rs` — encryption/redaction/rotation, owner-authorized internal endpoints, redirect/DNS policy |
| A08 Serving context | pass | `fallbacks.rs`, `experiments.rs`, `cache.rs`, `serving_catalog.rs`, `serving.rs` — fallback preserves customer/permissions; cache keys bind full serving context (T07.b); no new inference cost on cache hit |
| A09 Prices | pass | `business_prices.rs`, `terms.rs` — precedence/scheduling/overlap/discount/rounding/overflow/immutability |
| A10 Entry/price proofs | pass | `usage_reports.rs`, `witness.rs`, `evidence_bundles.rs` (11 `oxsum_verify` + 9 HTTP tests); packed consumer rejects altered rate/amount/funding/customer/signer |
| A11 Operation recovery | pass | `coordinated.rs`, `requests.rs`, `tasks.rs` — same-fingerprint replay, changed-input conflict, crash-before/after commit, no blind retransmission |
| A12 Ledger transaction | pass | T11 (PR #658) one-ledger transaction API + `heads.rs`, `journals.rs` — fault rollback leaves no entry/leaf/outbox; replay once; 33 engine Postgres conformance tests executed in final manual CI |
| A13 Zero-cost usage | pass | `entitlement.rs`, plan-coverage cells in `plans.rs` — covered request succeeds on zero wallet, no fabricated purchased hold; receipt not revenue |
| A14 Bounds and day close | pass | `coordinated.rs`, `sweep.rs`, `usage.rs` — concurrent-key caps, cancellation explicit, sealed period immune to delayed projection |
| A15 Protocol matrix | pass (stub surfaces); external-pending (real providers) | `chat_flow.rs`, `gateway.rs`, `apis.rs`, `metering` suites; `real_upstream*.rs` are opt-in and require provider credentials — one historical DeepSeek nonstreaming call has evidence in T31-followup.md; streaming/retry/fallback and remaining real providers are unverified |
| A16 Payment trust | pass | `webhooks.rs`, `merchant_events.rs`, `stripe.rs`, `hyperswitch.rs`, `airwallex.rs`, `x402.rs` — wrong signature/merchant/currency/amount/customer refused; duplicate/reordered callback once; rotation |
| A17 Order fulfillment | pass | `payment_orders` flow in `merchant_events.rs`/`webhooks.rs` — paid-unfulfilled recovery, body-change conflict, no re-charge, purpose isolation, redirect never credits; #693 scheduler regression covers startup/five-second recovery even when daily reconciliation is scheduled tomorrow |
| A18 Refund | pass (stub); external-pending (provider money-out) | `refunds.rs` + `faults.rs::a_refund_retry_storm_releases_at_the_attempt_budget` — bounded totals, concurrent consume safe, reservation release, money-out once |
| A19 Corrections/disputes | pass | `refunds.rs`, `journals.rs`, dispute cells — originals retained, promo not cash-refunded, distinct dispute/write-off/refund audits |
| A20 Grants | pass | `promo_lots.rs` + `faults.rs` promo pin/expiry cell — conservation under race, legacy bonus reconciled, bundle replay once |
| A21 Plans | pass (lifecycle); external-pending (paid activation against a live PSP) | `plans.rs`, `plan_statements.rs`, `entitlement.rs` — renewal/cancel/boundary/period-race/leap clamp/zero-wallet included use |
| A22 Plan accounting | pass | `journals.rs`, `plan_statements.rs` — deferred/earned conservation, integer remainder, replay-once recognition, quota not extra fee |
| A23 Credit/statements | pass | `plan_statements.rs`, `statements.rs`, `bills.rs` — credit bounds, finalize once, immutable snapshot, partial allocation, overdue restore only for dunning suspension |
| A24 Reports | pass | `financial_facts.rs` — customer totals reconcile to business sum, plan not double-counted, unknown vs zero cost, provider/day granularity, replay same facts |
| A25 Browser journeys | scoped pass | [closeout-browser.md](evidence/closeout-browser.md): owner/context and staff appointment, API-prepared invitation with browser registration, two stub purchases, key, billed stream, actual exported bill accepted/wrong hash rejected, customer/finance refund decisions, support denials, mobile/keyboard/error/empty states. Ten inspected screenshots. Setup/provider provisioning was API-driven; plan/dispute/receivables browser cycles and a complete accessibility audit remain unexecuted. |
| A26 Independent evidence | pass | `evidence_bundles.rs`, `verify` crate 11 tests, mandatory candidate `check-verify-consumer.sh` plus the candidate SDK verifying the browser journey's actual retained bill — genuine verifies offline; tampered/missing/wrong-origin rejected; `scope.excluded` names honest limits |
| A27 Integrations | pass (local); external-pending (certified-provider examples) | `examples_surface.rs` (3/3), `verify-examples.sh` rerun on the exact candidate (consumption, scoped-management and webhook tools), `docs/integrations.md` + `docs/unsupported.md` cite proving suites; real-provider example calls need credentials |
| A28 Fault/capacity | functional pass; performance target missed | [T27.md](evidence/T27.md) fault suites plus [closeout-capacity.md](evidence/closeout-capacity.md). Actual 1,000-account/10,000-key/100-stream workload: 5,897/0 successful/failed, drift 0, open holds 0. Admission 19.657/s, p95 overhead upper bound 113.421 ms misses >=20/s and <100 ms. Development hardware, not 4-vCPU certification. |
| A29 Install/restore | pass | `install_ops.rs` 7/7 — fresh install, previous-version fixture upgrade, cross-database backup/restore preserves state+keys, bad config diagnosed; runbooks `docs/self-hosted-install.md`, `docs/self-hosted-upgrade.md` |
| A30 Candidate artifact | executed artifact checks; scoped browser evidence | [closeout-candidate.md](evidence/closeout-candidate.md): exact-SHA full CI including all heavy jobs and engine conformance, packed consumer, image startup/version equality, exported image and manifest/checksums, no publication. The interactive browser session used incrementally corrected local builds, documented under A25; it was not a complete journey against the final Docker image. |
| A31 External launch | external-pending | By contract A31 evidence must come from authorized outsiders; prerequisites enumerated in RELEASE-CHECKLIST §External actions |

## Financial invariants and limits

The named ledger/domain suites assert conservation, capped funding allocation,
entitlement separation, idempotent payment fulfillment, bounded refunds and
immutable closed history. The measured streaming run additionally reconciles
each wallet, all operation/usage charges and zero remaining reservations.
The browser fixture independently reconciles 3,000,000 purchased minus
2,000,000 refunded minus eight usage units to 999,992 available, zero reserved.
These are executed checks within their fixtures, not a proof over all future
production executions.

## Non-claims

- D09 throughput/overhead targets were not achieved; four-vCPU and prolonged
  production-load behavior remain unmeasured. The raw JSON/log is retained.
- Browser scope is listed explicitly in A25. API tests and a successful UI
  build do not establish all browser workflows or full accessibility.
- No closeout payment moved money through a real PSP. The older Airwallex
  experiment manually inserted a provider reference, submitted a locally
  signed webhook and reversed the ledger; provider capture/refund did not run.
- The historical DeepSeek result is one nonstreaming call. It does not close
  the multi-provider/protocol certified-example matrix.
- No independent pilot was simulated, and this work publishes no release,
  container image or npm package and performs no live deployment.
