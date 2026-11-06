# Final acceptance results — T30

- Issue: [#621](https://github.com/AllureCurtain/oxsum/issues/621). Contract: [ACCEPTANCE.md](ACCEPTANCE.md) (scenarios A01–A31, financial invariants, promotion gates G1–G5).
- Candidate under acceptance: `main` at `45e4b22` (T29 merge) — the release-candidate workflow and smoke script are integrated; the matrix below covers the full integrated software. The T29 *candidate run* itself is a maintainer dispatch (gate 2 in `docs/self-hosted-release.md`), not something this acceptance executes.
- Environment for locally executed checks: `local-isolated`, macOS arm64, Rust stable, PostgreSQL 17 in Docker (`oxsum-postgres-1`), per-suite `oxsum_test_*` scratch databases. No live provider or payment credential was used; every external dependency ran its stub/harness path.
- Verdict summary: **A01–A30 pass on integrated evidence (A30's live candidate run remains a maintainer dispatch, not an executed artifact); A31 external-pending.** No unresolved money, permission or isolation defect is open. This is not a production-ready claim — G4/G5 remain unexercised by policy.

## What was executed for this matrix (2026-11-06, this worktree)

| Check | Command | Result |
| --- | --- | --- |
| Formatting | `cargo fmt --all --check` | pass — clean tree |
| Shell syntax | `bash -n` over `verify-examples.sh`, `check-verify-consumer.sh`, `check-config.sh` | pass |
| Journey suites | `cargo nextest run -p oxsum-server --test business_console --test examples_surface --test install_ops --test evidence_bundles` | **22 passed, 0 failed, 1 skipped** — the skip is `evidence_bundles::dump_v2_fixture`, `#[ignore]`-marked fixture regeneration, not a test |
| Clean consumer | `scripts/check-verify-consumer.sh` | pass — `verify-oxsum-bill@0.1.0` packs, installs clean, verifies the genuine v2 bundle, rejects tampered / missing-row / wrong-org / wrong-key / wrong-archived-head mutations; `fetch was disabled and never called` |

The heavy tier (three nextest shards, process-shared `generative`/`seed`/`jobs`, doubleentry Postgres conformance) runs per-push on `main` — the candidate SHA inherits its last post-merge run rather than a re-run here; the T29 runbook names it gate 0.

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
| A12 Ledger transaction | pass | T11 (PR #658) one-ledger transaction API + `heads.rs`, `journals.rs` — fault rollback leaves no entry/leaf/outbox; replay once; doubleentry Postgres conformance runs post-merge on `main` |
| A13 Zero-cost usage | pass | `entitlement.rs`, plan-coverage cells in `plans.rs` — covered request succeeds on zero wallet, no fabricated purchased hold; receipt not revenue |
| A14 Bounds and day close | pass | `coordinated.rs`, `sweep.rs`, `usage.rs` — concurrent-key caps, cancellation explicit, sealed period immune to delayed projection |
| A15 Protocol matrix | pass (stub surfaces); external-pending (real providers) | `chat_flow.rs`, `gateway.rs`, `apis.rs`, `metering` suites; `real_upstream*.rs` are opt-in and require provider credentials — external half of A15 listed under external actions |
| A16 Payment trust | pass | `webhooks.rs`, `merchant_events.rs`, `stripe.rs`, `hyperswitch.rs`, `airwallex.rs`, `x402.rs` — wrong signature/merchant/currency/amount/customer refused; duplicate/reordered callback once; rotation |
| A17 Order fulfillment | pass | `payment_orders` flow in `merchant_events.rs`/`webhooks.rs` — paid-unfulfilled recovery, body-change conflict, no re-charge, purpose isolation, redirect never credits |
| A18 Refund | pass (stub); external-pending (provider money-out) | `refunds.rs` + `faults.rs::a_refund_retry_storm_releases_at_the_attempt_budget` — bounded totals, concurrent consume safe, reservation release, money-out once |
| A19 Corrections/disputes | pass | `refunds.rs`, `journals.rs`, dispute cells — originals retained, promo not cash-refunded, distinct dispute/write-off/refund audits |
| A20 Grants | pass | `promo_lots.rs` + `faults.rs` promo pin/expiry cell — conservation under race, legacy bonus reconciled, bundle replay once |
| A21 Plans | pass (lifecycle); external-pending (paid activation against a live PSP) | `plans.rs`, `plan_statements.rs`, `entitlement.rs` — renewal/cancel/boundary/period-race/leap clamp/zero-wallet included use |
| A22 Plan accounting | pass | `journals.rs`, `plan_statements.rs` — deferred/earned conservation, integer remainder, replay-once recognition, quota not extra fee |
| A23 Credit/statements | pass | `plan_statements.rs`, `statements.rs`, `bills.rs` — credit bounds, finalize once, immutable snapshot, partial allocation, overdue restore only for dunning suspension |
| A24 Reports | pass | `financial_facts.rs` — customer totals reconcile to business sum, plan not double-counted, unknown vs zero cost, provider/day granularity, replay same facts |
| A25 Browser journeys | pass | `business_console.rs` (3 journeys), `browser_contract.rs`, `spa.rs`, `dashboard.rs`, `branding.rs`; console/portal sections all mounted behind capabilities; T23 portal + T24 console exercised via HTTP contract + built frontend (`frontend` CI job builds `web/dist` every PR) |
| A26 Independent evidence | pass | `evidence_bundles.rs`, `verify` crate 11 tests, `check-verify-consumer.sh` re-run above — genuine verifies offline; tampered/missing/wrong-origin rejected; `scope.excluded` names honest limits |
| A27 Integrations | pass (local); external-pending (certified-provider examples) | `examples_surface.rs` (3/3), `verify-examples.sh` full run green on T26 (consumption 14, scoped-management 11, webhook-tools all rails), `docs/integrations.md` + `docs/unsupported.md` cite proving suites; real-provider example calls need credentials |
| A28 Fault/capacity | pass with declared gap | `evidence/T27.md` — 17 cells green, measured schema-count/fairness/cache figures; **residue: normalized D09 shape (1,000 customers / 100 streams) and 5-min soak not run on target hardware** — scoped as release limitation, see RELEASE-CHECKLIST |
| A29 Install/restore | pass | `install_ops.rs` 7/7 — fresh install, previous-version fixture upgrade, cross-database backup/restore preserves state+keys, bad config diagnosed; runbooks `docs/self-hosted-install.md`, `docs/self-hosted-upgrade.md` |
| A30 Candidate artifact | pass | `release-candidate.yml` landed at merge `45e4b22`: same-SHA resolve → fast tier → wasm → frontend → verify-consumer → examples → image build (never pushed) → `candidate-bundle` (manifest + SHA256SUMS + notes draft). The gate's own run is a maintainer `workflow_dispatch` (gate 1 in `docs/self-hosted-release.md`), and live smoke `scripts/release-candidate-smoke.sh` is gate 2 — both documented, neither executed by this acceptance. Local halves re-verified above. No publication claim |
| A31 External launch | external-pending | By contract A31 evidence must come from authorized outsiders; prerequisites enumerated in RELEASE-CHECKLIST §External actions |

## Financial invariants — status

All invariants hold on the integrated candidate, enforced at the ledger layer and re-asserted by the fault matrix: journal balance conservation, capped charge = funding allocation sum, entitlement distinct from cash, one fulfillment per payment purpose, refunds bounded incl. pending, grant conservation `remaining + reserved ≤ grant` (T27 correction), closed history immutable (trigger-enforced on `invoice_journals`/`plan_statements`), analytics rebuild without money changes, v1 proof semantics intact (version dispatch, `WrongVersion`).

## Known gaps and non-claims

- **D09 normalized load is unmeasured at target scale.** T27's per-unit figures (11+26 catalog objects per tenant schema, p99 259 ms fairness bound on a 16-conn pool) are real; the 1,000-customer/100-stream shape is a projection, not a measurement. `scripts/soak.sh` exists unrunnable-to-completion locally. Recorded as a release limitation, not a hidden failure.
- **Browser-journey evidence is contract-level.** A25 rests on the HTTP contract suites plus the per-PR frontend build; no recorded pixel-level journey artifact ships in this matrix. The console/portal surfaces are exercised end-to-end through the API they consume.
- **No payment rail has moved real money.** A16–A18 cover trust boundaries and stub/provider-sandbox paths exhaustively; live certification is A31's job and stays external-pending.
- **`verify-oxsum-bill` is unpublished on npm.** The packed-tarball path is the verified distribution; publication is a G5 action.
