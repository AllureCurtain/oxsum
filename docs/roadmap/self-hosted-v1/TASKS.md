# Ordered task cards

IDs are package identifiers, not GitHub issue numbers. Create/reuse matching issues before implementation and store URLs in progress.json. Each card is a feature outcome with acceptance, not a license for a huge PR. Split a card into `Txx.a`, `Txx.b` issues if needed; preserve dependencies, map each TODO item to one issue, and keep each PR focused. No task is implemented merely because similar source exists.

Common done rule: contract first where needed; implementation, relevant tests and docs; source review; recorded exact commit/check evidence; focused PR integrated through normal green checks. An external-only gate can remain pending while ready software work continues.

## T00 — Adopt the release contract and baseline

- Depends: none. Type: docs.
- Read root/nested instructions, current TODO/product/architecture/decisions and this package. Refresh main safely; inspect local changes, worktrees, issues/PRs and current gates. Baseline had open issue #506 (provider OAuth import) and no PR; do not assume unchanged.
- Create umbrella design issue with R01–R12 and D01–D09, then focused issue cards in dependency order. Carry accepted spec into `docs/roadmap/self-hosted-v1/`; append positioning/role/compatibility decisions without deleting history. Update TODO with per-issue status links.
- Record D01 default (existing Organization = customer account, new Business = seller), route ownership, unsafe legacy privilege removals, single-business release scope and explicit external release gates.
- Verify no duplicate task of an existing open issue. Exit: design/docs PR and populated progress with real issue IDs, no invented completion state. No broad research loop.

## T01 — Restrict financial and metering authority

- Depends: T00. Type: fix. Sources: server routes/billing, auth, core sessions/service credentials.
- Audit existing topups, adjustments, credit notes, settlement/metering, refunds, credit limits and paid-status endpoints. Customer session/consumption key cannot mint money or assert arbitrary usage. Initially use existing deployment authority; T03 adds staff capabilities.
- Preserve gateway behavior and legitimate provider-confirmed deposits. Replace customer manual top-up UI with payment/request semantics. Explain breaking removal in release notes.
- Tests: A01/A02; authenticated customer/key denial leaves entries/balance unchanged; operator/manual receipt success audited. Do not merely hide the button.

## T02 — Add business and customer ownership without moving ledgers

- Depends: T00. Type: feat. Sources: core migrations/db/orgs/tenants, D01/D08.
- Add Business, customer_profiles and ownership mapping. Preserve existing organization/tenant/key IDs. Map global config to default business, idempotent rerun. Explicit setup appoints owner; no customer owner auto-promotion.
- Provide migration preview/receipt and one-business constraint. Map current organization/customer terminology through API documentation without renaming old wire fields gratuitously.
- Tests: A03 legacy migration fixture and cross-customer lookup; schema/cache scale smoke. Exit: old proofs/balances/keys unchanged, new accounts belong to correct business.

## T03 — Business staff, setup and session contexts

- Depends: T01,T02. Type: feat.
- Implement owner/admin/finance/support/viewer capability checks from PRODUCT. Separate business session authority from customer memberships. Protect owner transfer/last owner and privileged settings. One-time authenticated init, restart-safe completion and audit.
- Registration setting defaults invite-only. Setup token is one-use/expiring and not publicly claimable after initialization. Staff enrollment is invitation/owner controlled.
- Tests: A01/A04 privilege matrix, same user switching business/customer context, setup races, owner transfer, revoked staff/session handling. UI can follow in T24; core/API works independently.

## T04 — Customer profile and team lifecycle

- Depends: T02,T03. Type: feat. References: OpenMeter customer, Bifrost ownership.
- Customer external ID/contact/name, list/detail/invite/self-registration policy, archive and separate manual/dunning suspension. Reuse organization member/key semantics for customer teams; enforce appropriate user consent when joining.
- Rename never moves usage; archive does not discard debt/history. Customer signup resolves default business and creates customer account only. Support member invitation/role changes under customer owner/admin rules.
- Tests: A04/A05 identity collision, archive/rename, invited versus public registration, foreign account access, preservation of active operation identity.

## T05 — Scoped consumption and integration credentials

- Depends: T03,T04. Type: feat.
- Separate consumption from business-management/metering scopes. Hash secrets, one-time display, expiry/revocation, customer allowlists. Retain existing gateway keys safely; all legacy read/money scopes explicitly assessed.
- Scoped integration may select authorized customer; `endUser` stays attribution. Inventory all APIs/export/websocket/proof/task accesses and create enforced capability coverage tests.
- Tests: A01/A02/A06; credential mismatch cannot debit, read or replay another account; no secret in logs/response except creation.

## T06 — Business-owned provider configuration

- Depends: T02,T03. Type: feat.
- Scope channel/credential/alias/catalog/routing configuration to Business; existing resources mapped compatibly. Connection checks require explicit test action. Preserve key encryption/rotation/pool behavior.
- Allowed destination policy handles redirects/DNS/private endpoints and credential forwarding. Self-hosted owner can authorize its internal endpoint; lower-privilege users cannot implicitly target server infrastructure.
- Tests: A07 authorization, endpoint policy, cross-config ownership, rotation and old in-flight route references. Reuse current channel code; no new proxy infrastructure required.

## T07 — Model catalog and private serving context

- Depends: T05,T06. Type: feat.
- Expose customer-allowed models only. Bind route/fallback/experiment/cached/task contexts to admitted customer and permissions. Keep cache organization ID as customer account scope; version incompatible keys.
- Ensure aliases, fallback and experiments cannot widen model/channel access. Preserve configured original requested/served names.
- Tests: A06/A08 same model across customers, tool/context-sensitive cache keys, async/task status isolation and permission-denied fallbacks. Document exact protocol matrix starter.

## T08 — Versioned customer sale books

- Depends: T02,T04,T06. Type: feat. References: Meteroid price domain/Lago.
- Add book/version/assignment/override model with defaults and effective windows. Separate sale terms from supplier costs. Draft/publish history, price preview and scheduling; reject overlapping ambiguous terms.
- Reuse pure existing itemized/rule rating. Preserve legacy channel price versions as legacy sale snapshots where required.
- Tests: A09 precedence, condition coverage, effective instant, overflow/rounding/discount, unknown dimension and immutable versions. No float conversion.

## T09 — Bind sale terms into evidence

- Depends: T08. Type: feat.
- Canonical published sale snapshot/digest and signature binding; new proof/settlement version for customer price identity. Update wasm/SDK types and signed-catalog endpoints coherently.
- Old versions verify unchanged; new customer evidence excludes supplier rates. Snapshot customer/billing display fields separately from proof-sensitive identity.
- Tests: A10 old/new positive fixtures and altered price/discount/rate/customer/signer failures, independent pure verifier build. Avoid claiming a new price is proven by an old wholesale/channel proof.

## T10 — Durable request operation

- Depends: T05,T07,T08. Type: feat.
- Add operation/attempt/funding context and scoped idempotency fingerprint, lease/heartbeat/CAS, pinned customer/terms. Use same context for streaming, non-streaming and tasks. Define terminal outcomes/recovery interface.
- Persist enough observed final usage or explicit estimate before charging; no prompt bodies in ledger. Unknown inference outcome never triggers blind retransmission.
- Tests: A11 replay under changed customer/input/terms, in-flight ownership changes, lease race, termination after provider acceptance. Operation migration is backward-compatible with old open holds.

## T11 — Transaction-aware ledger effects and zero-cost receipts

- Depends: T10. Type: feat/build foundation; split if necessary.
- Prototype and implement D05 narrow one-ledger transaction API preserving append behavior, limits, MMR/sequence and rollback; include failure injection. Update old wrapper/tests and document decision/engine markers.
- Implement typed net-zero usage receipt for free/fully entitled operations, without 1-minor purchased balance demand. Update settlement classification and hold/recovery assumptions; receipt is not revenue.
- Tests: A12/A13 plus all doubleentry tests incl Postgres; forced rollback leaves no entry, proof leaf, allocation or outbox; old append concurrency still passes. If API infeasible record durable alternative and equivalent failure tests, not a best-effort fallback.

## T12 — Coordinated customer admission and settlement

- Depends: T09,T10,T11. Type: feat.
- Coordinate customer/key/member budgets, money reservation, state, settlement and essential projection/outbox. Pin funding and retain integer cap invariants. Integrate sweeper/recovery with operation state and real stream heartbeat.
- Close/seal uses known watermark; rebuildable analytics failures do not re-charge. Legacy hold compatibility is explicit.
- Tests: A11–A14; concurrent keys, cancellation, excessive/missing usage, crash points and midnight recovery. Assert balance/ledger/operation invariants, not only response status.

## T13 — Cover every serving and metering path

- Depends: T12. Type: feat; may split by protocol if independently complete.
- Connect Chat, Responses, Messages, Gemini, embeddings/rerank, cache, fallback/experiments, generic async tasks and privileged event metering to operation/rating/evidence boundaries. Keep SDK-compatible errors.
- Cost records distinguish real attempt, cache reuse, estimate, missing and provider-reconciled allocation. Upstream may incur cost on failed attempts; do not fabricate zero.
- Tests: A08/A14/A15 scripted provider matrix, pre-byte retry/post-byte cancellation, task completion/replay and metering scope. Existing real-provider tests remain opt-in and truthful.

## T14 — Merchant settings and scoped event intake

- Depends: T03,T04. Type: feat. Reference: Hyperswitch.
- Business-owned encrypted/versioned merchant configuration, test/live separation, validation/rotation and webhook signing context. Adapt env bootstrap without exposing secrets. Provider event scope includes merchant identity.
- Primary checkout/refund contract: Hyperswitch Stripe connector. Preserve other rails with tested/experimental status.
- Tests: A16 wrong merchant/signature/currency/customer, duplicate IDs across merchants, rotation and webhook before synchronous response.

## T15 — Payment orders and durable fulfillment

- Depends: T11,T14. Type: feat.
- Immutable purpose/order terms, server-rated currency conversion, provider idempotency, state/outbox/reconciliation. Separate paid from fulfilled; purpose-specific fulfillment cannot both grant quota and wallet cash.
- Recovery scans paid-unfulfilled orders; no retry issues a new provider charge. Match exact expected amount/currency and use explicit discrepancy state.
- Tests: A16/A17 callback race/timeout/replay, wrong amount, checkout URL replay and source reference uniqueness. Customer return page cannot mark paid.

## T16 — Customer checkout and purchased funding

- Depends: T04,T05,T15. Type: feat.
- Customer amount/order/checkout/status/history APIs and primary connector integration. Fulfill wallet top-up once; expose pending/failed/expired truth. Manual receipt is authorized finance action with unique evidence.
- Tests: A17/A18 real local checkout stub journey and sandbox preparation, repeated browser submission, failed provider and duplicate callbacks. UI details in T23; working API flow required here.

## T17 — Corrections, partial refunds and disputes

- Depends: T12,T16. Type: feat. References: Blnk/Lago/Hyperswitch.
- Credit note original funding reversal, bounded refundable/pending amounts, reservation before PSP call, durable retry and definite-failure release. Preserve original entries and link correction/refund/provider event.
- Customer can request review, not grant own refund. Audit finance resolution. Dispute policy distinct from successful refund.
- Tests: A18/A19 concurrent consume/refund, partial total, duplicate/reordered webhook, provider timeout, spent/promotional funds and failed compensation. No silent negative wallet.

## T18 — Promotional lots and prepaid bundles

- Depends: T12,T15. Type: feat.
- Grant lots/expiry with immutable source events, pinned hold allocation and deterministic draw/release. Bundle purchase posts purchased credit and separate promo benefit under one purpose fulfillment.
- Expiry and correction cannot turn promo into refundable cash. Migrate aggregate legacy bonus to non-expiring legacy lot without changing net balances.
- Tests: A20 expiry during hold, concurrent expiry/settlement, replay grant, refunded bundle and zero purchased balance. Old bonus totals reconcile.

## T19 — Customer entitlement and paid plan lifecycle

- Depends: T12,T15,T18. Type: feat; split lifecycle/checkout if needed.
- Versioned sold plans, one active plan, purchased authorization per period, renewal order, boundary cancellation/change, expired/past-due, opt-in overage. Reuse legacy quota machinery with pinned period events.
- Implement D06 validity; timer cannot issue unpaid entitlement. Free/complimentary grants explicit. Preserve legacy calendar boundary policy; new anniversary policy versioned.
- Tests: A21 purchase/renewal failure/cancel/period rollover/concurrency/leap-month clamp, 100% covered request with zero wallet, old draw recovery and allowance expiry.

## T20 — Plan fee journals and revenue allocation

- Depends: T17,T19. Type: feat.
- Define/document additive plan deferred/earned account postings and integer recognition schedule. Separate plan payment from wallet funding, quota usage from extra charge. Handle credit/refund of unearned/earned share with linked immutable correction.
- Reuse ledger authority, unique recognition day/period keys and transactional outbox. Legacy amounts remain interpretable.
- Tests: A22 exact fee conservation/daily remainder, partial-period refund, replay job, no double counting plan face-value usage, old schema/proof compatibility.

## T21 — Customer credit, statements and overdue handling

- Depends: T04,T12,T17,T20. Type: feat.
- Adapt existing credit line/statement/payment allocation/dunning to staff/customer roles. Snapshot documents, due dates, partial payment, reasoned corrections, FIFO or explicit deterministic allocation. Finance credit approval only.
- Payment order purpose repays receivable once; suspension origin prevents automated lift of manual hold. Final statement does not change on customer rename.
- Tests: A23 full cycle including partial payment, overdue restoration, concurrent finalization and capped credit reduction. Export and proof reflect stored statement.

## T22 — Financial facts, cost coverage and reconciliation

- Depends: T13,T17,T20,T21. Type: feat.
- Expose separate usage/rated/entitlement/funding/earned/collected/refund/receivable amounts with customer/model/time filters and booking/service/payment time definitions.
- Costs handle unknown vs zero, failed attempts, cache and provider/day granularity. Reconciliation has run/evidence/exception/resolution; manual repair is explicit financial action, not balance overwrite.
- Tests: A24 conservation across report totals, period seams, unique provider totals, plan revenue, replay/rebuild and incomplete-cost margin. Customer totals reconcile to business sum.

## T23 — Customer self-service portal

- Depends: T04,T05,T09,T13,T16,T17,T19,T21. Type: feat; split by complete customer journey.
- Registration/invite/login, seller branding, price/plan view, purchase/renew/cancel, keys/SDK setup, usage/bills/payments, refund request/help. Reuse dashboard components and customer-team model.
- All pages enforce backend permissions, show actual money versus allowance and correct loading/empty/error/expired states. No manual credit or privileged token exposure.
- Tests: A25 browser journeys desktop/mobile/keyboard; real local stub payment/request/evidence. API tests alone do not close UI acceptance.

## T24 — Business console and operational resolution

- Depends: T03,T04,T06,T08,T17,T21,T22. Type: feat; split resource/flow PRs.
- Setup checklist; customer details; channels/prices/plans; payments/refunds; statements/receivables; reports/exceptions; staff/integrations/audit. Role-aware navigation, explicit business/customer context.
- Drill from request to price/allocations/entry/payment; recovery actions audited and idempotent. Follow DESIGN.md, existing locales/components.
- Tests: A25 role-specific browser journeys, forms/errors/responsive tables, finance correction and support denial. No hidden dependency on database console.

## T25 — End-to-end customer evidence and compatibility

- Depends: T09,T13,T17,T20,T21. Type: feat/test.
- Complete report/day close, statement/plan/adjustment evidence, legacy verification, customer-safe downloads/share expiry and independently usable bundle/SDK. Describe each claimed scope honestly.
- Tests: A10/A26 positive genuine bundles and tampered amount/rate/funding/customer, missing committed row, wrong key/origin, changed historical names and unanchored history. No supplier cost leakage.

## T26 — Supported integrations and developer examples

- Depends: T05,T13,T15,T19,T25. Type: feat/docs.
- Publish exact protocol/provider/parameter and payment support tables; runnable scoped-management/metering and consumption examples, webhook signature/replay/dedup tools. Use actual generated contract types.
- Verify new user examples from a clean consumer/project and actual package. Do not claim extra modalities because async API exists. Issue #506 stays out of scope unless user later changes scope.
- Tests: A27 SDK smoke, wrong scopes, webhook retries, fresh-install examples and documented unsupported behavior.

## T27 — Cross-cutting fault, isolation and capacity hardening

- Depends: T13,T17,T18,T19,T20,T21,T22,T25. Type: test/fix.
- Run failure-injection matrix, route authority coverage, secret/log review, cache/task/export isolation and shared-pool concurrency. Resolve failures through focused fix issues, preserving required gates.
- Benchmark D09 workload with declared infrastructure; measure schema count/cache memory and busy-customer fairness. Run controlled soak and job retry storms.
- Tests: A01–A24/A28; report exact commit/workload/result/artifact. Do not weaken tests to produce green or treat skipped DB suite as passed.

## T28 — Self-hosted installation, upgrade and restore

- Depends: T02,T03,T14,T20,T21. Type: build/feat/docs.
- Versioned core/payment Compose profiles, initialization and secure secrets instructions, health/readiness, operator configuration, backup/restore including keys, migration/rollback runbook and visible version.
- Production demo data disabled by default. Existing deploy workflow is manual and must not be triggered as a test. Extract publish-only release path from deployment side effects where needed.
- Tests: A03/A29 fresh setup, previous-version fixture upgrade, separate-environment restore/recovery and invalid secret/config diagnostics. Measure recovery, don't promise untested RPO/RTO.

## T29 — Release artifact and candidate gates

- Depends: T25,T26,T28. Type: ci/build/docs.
- Candidate workflow requires full relevant integration/engine/frontend/wasm checks on exact promoted SHA; align AGENTS/docs/CI policy explicitly. Build images, checksums/provenance, notes, SDK tarball and deterministic consumer validation.
- Keep publishing and production deployment separate/manual. Prepare release artifacts without invoking credentials unnecessarily.
- Tests: A30 candidate artifact smoke with customer/business surfaces and positive/negative browser verifier; existing `check-verify-consumer.sh`; clean packaging. No artifact publication claim until verified remote result.

## T30 — Final integrated acceptance and launch materials

- Depends: T23,T24,T26,T27,T28,T29. Type: test/docs.
- Run full matrix on integrated candidate, operator/customer end-to-end journeys, documented independent onboarding dry run, isolated demo fixtures, accurate README/screens and support/security reporting guides.
- Record every gate as pass/fail/external-pending with evidence. Update roadmap/TODO/progress/release checklist. Resolve software failures before calling implementation complete.
- Exit: validated software candidate, reviewable release materials and an exact external-action list. No fabricated production-ready label.

## T31 — External certification and promotion gates

- Depends: T30. Type: release validation; external-dependent.
- With separately authorized merchant/test credentials, complete sandbox and controlled low-value live payment/refund certification; with real providers validate supported certified examples. Coordinate actual independent pilot operators only when user authorizes contact.
- Two outside users complete installation plus billing/plan cycle or documented time-advance validation. Actual public release/publish/deploy is authorized separately after exact candidate is reviewable.
- Tests: A31 and external portions of A15/A18/A21/A27. Record who performed/when/artifacts without secrets or personal data. If unavailable, mark external_pending with exact prerequisites; do not fabricate pilot results or block earlier software tasks.
