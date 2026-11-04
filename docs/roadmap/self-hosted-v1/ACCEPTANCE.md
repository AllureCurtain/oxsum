# Verification and evidence contract

Every task links to scenario IDs below. Passing requires exact commit, command/harness, environment, outcome and evidence location. Keep secrets/private payloads/live customer records out of evidence. A skipped database test is skipped, not passed.

## Scenario matrix

| ID | Scenario | Required assertions |
| --- | --- | --- |
| A01 | Capability matrix | Customer/session/key/staff/integration authority for every route; denied money action changes no balance/entry |
| A02 | Forged funding/usage | Customer cannot self-top-up/adjust/meter/confirm refund; checkout creates pending order only |
| A03 | Migration | Old IDs/balances/roots/origins/statements/proofs/keys unchanged; rerun idempotent; customer never auto-promoted to staff |
| A04 | Setup and membership | Setup races/expiry, invite-only/open signup, revoked access, last-owner transfer, user context switch, customer-team invitation |
| A05 | Identity stability | Rename/archive/reassign cannot move in-flight/history; suspension blocks new admissions but allows recovery/refunds |
| A06 | Isolation | Foreign raw entries/proofs/exports/websocket/cache/task/payment/order/report/replay denied; staff and customer roles independent |
| A07 | Provider setup | Encryption/redaction/rotation; owner-authorized internal endpoints; redirects/DNS/credential forwarding obey destination policy |
| A08 | Serving context | Fallback preserves customer/permissions; same-model sale stable; cross-model bounds; cache context complete; no new inference cost on cache hit |
| A09 | Prices | Scheduling/precedence/overlap/discount/units/rounding/overflow/unknown dimension; immutable in-flight/historical terms |
| A10 | Entry/price proofs | Genuine legacy/new verify; altered rate/amount/discount/funding/customer/signer fails; no supplier secrets |
| A11 | Operation recovery | Same fingerprint replay; changed input/customer conflict; crash before/after send/response/commit; unknown inference not blindly retried |
| A12 | Ledger transaction | Fault after rows/MMR/quota/outbox rolls all back; committed replay once; old wrapper/cross-process append remains correct |
| A13 | Zero-cost usage | Zero wallet/free or covered plan succeeds without fabricated purchased hold; receipt changes no balance/revenue |
| A14 | Bounds and day close | Concurrent keys/member/team cannot overspend; cancellation/cap/estimate explicit; delayed projection cannot corrupt sealed period |
| A15 | Protocol matrix | Chat/Responses/Messages/Gemini/embeddings/rerank/tasks/metering cover success/error/timeout/cancel/retry/usage/receipt; real vs stub labeled |
| A16 | Payment trust | Wrong signature/merchant/currency/amount/order/customer refused; callback races/duplicate/reordering once; rotation valid |
| A17 | Order fulfillment | Paid-unfulfilled recovery, changed-body conflict, no new provider charge on retry, purpose isolation; redirect never credits |
| A18 | Refund | Total incl pending bounded; concurrent consume safe; ambiguity retains reservation; failure releases; success money-out once |
| A19 | Corrections/disputes | Originals retained; promo not cash-refunded; original funding restored; disputes/write-offs/refunds distinct and audited |
| A20 | Grants | Legacy bonus conserved; expiry order; reserved source survives expiry, expired release not revived; bundle/replay once |
| A21 | Plans | Paid activation/renewal, no unpaid refill, boundary cancellation/change, overage opt-in, month-end policies, period race and zero-wallet included use |
| A22 | Plan accounting | Deferred/earned/refunded fee conservation, integer remainder, replay recognition once; quota use not extra fee |
| A23 | Credit/statements | Credit bounds, finalize once, immutable snapshot, partial/full allocation, overdue restoration only for dunning suspension |
| A24 | Reports | Customer totals sum, plan not double-counted, unknown vs zero cost, provider/day totals not multiplied; rebuild/replay same facts |
| A25 | Browser journeys | Built UI desktop/mobile/keyboard: setup/invite/register/pay/key/call/bill/refund and staff flows; empty/error/denied states |
| A26 | Independent evidence | Real exported statements/reports offline/browser verify; tampering/missing committed rows/wrong origin rejected; old/unanchored scope honest |
| A27 | Integrations | Clean consumer uses actual contract/package, scoped management/metering/SDK and signed webhook replay; unsupported paths documented |
| A28 | Fault/capacity | Multi-process/pool pressure/fairness/lease/retry/restart tests; measured D09 workload and soak without unexplained drift |
| A29 | Install/restore | Fresh install without DB edits; old fixture upgrade; restore DB plus keys/config/jobs; balances/hashes survive; bad config diagnosed |
| A30 | Candidate artifact | Same SHA image/verifier passes full gates/smoke/startup/version/browser positive-negative checks/clean consumer |
| A31 | External launch | Authorized merchant sandbox/live payment-refund, certified provider calls, two independent pilots; never simulate outside-user evidence |

## Financial invariants

Every journal balances and available funds stay authorized. Capped rated charge equals funding allocation sum, while entitlement is distinct from cash. Payment purpose has one fulfillment. Successful plus pending refunds remain bounded. Grant/period reserved/consumed/expired/released amounts conserve allowance. Closed history never rewrites. Analytics rebuild without money changes. Old proof semantics remain intact.

## Checks

Read current scripts/config before choosing flags. Known commands:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo nextest run -p oxsum-core
cargo nextest run -p oxsum-server
cargo test --doc --workspace
```

Use focused suites while iterating. Cross-crate changes require full runtime coverage. Current CI excludes process-shared `generative`, `seed`, `jobs` from nextest shards and runs each in one process; mirror current CI and do not silently omit them. Engine changes require all doubleentry tests including Postgres conformance. Determine exact features/commands from current manifests/CI.

Frontend: `npm ci`, `npm run codegen`, `npm run build` in web as relevant; check generated diff and actual built UI. Baseline web has no test script: add a meaningful browser harness where needed, never claim nonexistent `npm test` passed. Follow browser skill for UI verification.

Verifier: pure/wasm tests, `scripts/build-verify-wasm.sh`, `scripts/check-verify-consumer.sh` plus actual browser checks. Do not publish just to test packaging.

Use PostgreSQL 17 isolated databases/testkit, not production or destructive development resets. Confirm DB tests executed rather than auto-skipped without credentials; do not print secret URLs. Restore into a separate environment.

## Evidence format

Record task/scenario, candidate SHA/branch, environment (`local-isolated`, `stub`, `provider-sandbox`, `live-authorized`, `independent-pilot`), command/reproduction, expected invariant, observed result/count, artifact path/CI URL and remaining prerequisite. Summaries go in tracked `docs/roadmap/self-hosted-v1/evidence/`; large artifacts may link CI/local output. State passed/failed/skipped/external_pending exactly.

## Promotion gates

- G1: T00–T30 software validated/integrated; no unresolved money or permission/isolation correctness issue.
- G2: exact promoted SHA passes relevant full checks, built UI and independent packaged verifier.
- G3: measured capacity and fault/upgrade/restore evidence; accurate support matrix.
- G4: primary live collection certification and independent pilots with authorized real evidence.
- G5: user authorizes actual publication/deployment for the reviewable candidate, or expressly chooses a narrower preview release with limitations.

Without G4/G5 report software implementation complete and external verification pending, never publicly launched, certified or fully release-ready. Continue independent software work when external prerequisites are absent.
