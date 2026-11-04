# Implementation design defaults

These engineering defaults are adopted by #591; BASELINE.md records source verification. They resolve earlier open choices, not user-specified table names. If current source disproves a default, document a bounded alternative preserving acceptance and proceed. Do not reopen the software-provider funds flow or drop scope silently.

## D01. Preserve customer ledgers; add the seller

Existing `oxsum.organizations` represents a person/team owning members, keys, a wallet/ledger, statements and usage. Keep it as the **customer billing account**. UI and new business APIs call it Customer. Preserve organization/key/tenant IDs, balances, ledger origins and old signed evidence.

Add `businesses` and `business_staff`. One enabled Business per deployment is supported in release one; reject unsupported second-business creation. Map existing global channels/plans/collection to that Business on initialization. Add `customer_profiles` keyed by organization ID, with business FK, stable external ID, contact and lifecycle. Customer memberships remain existing organization memberships. A person may hold staff and customer memberships; request authority must select the appropriate context explicitly.

Initialize the business owner through a one-time authenticated setup path, never public registration. Existing signup can continue creating personal customer organizations; it must not create staff. Customer teams reuse owner/admin/member behavior. Organization owner means customer-account owner, not business owner.

Add BusinessContext to configuration/catalog/payment access without renaming every organization reference. Keep deployment-token APIs restricted to control/recovery. Add business session/capability APIs over shared core services. No new commercial layer or wholesale wallet.

This preserves current per-customer schema isolation. T02/T27 measure actual schema/cache scale. Do not rewrite to shared subaccounts unless measured need and a compatibility design justify it.

## D02. Persistence inventory

Choose migration numbers/module names from current code. New tables use oxsum migrations; doubleentry owns its tables.

| Resource | Fields/invariants |
| --- | --- |
| businesses | UUID, slug/profile/contact, registration mode, currency/conversion version, initialization state; one active business |
| business_staff | business/user/role/capabilities; protected owner transfer |
| customer_profiles | organization PK/FK, business FK, unique business/external ID, contact/state; stable ledger association |
| integration_credentials | business, hashed secret/prefix, scopes, optional customer allowlist, expiry/revocation; plaintext once |
| configuration ownership | business FK on channels/aliases/provider/routing/plans as needed; legacy global rows map to default business |
| price_books/versions | business/book/model/dimensions/conditions, immutable published versions/effective windows/digest; separate cost versions |
| price_assignments/overrides | customer/group/book, effective interval; deterministic winner at each precedence |
| billing_operations | UUID, business/customer/key/service, scoped idempotency/fingerprint, pinned terms/identity, state/version/lease/heartbeat, dates/entry refs |
| operation_attempts | operation/index, route/credential ref, provider request ID, outcome/ambiguity, usage/cost provenance; no secrets |
| funding_allocations | operation/source/lot/period, reserved/consumed/released, expiry; reconciles to charge and postings |
| payment_orders | customer/purpose/currency/amount/conversion, merchant version, provider ref/state/idempotency fingerprint |
| payment_events/outbox | verified merchant-scoped event ID/digest/status; durable same-key retry |
| refunds | source payment/customer/amount/currency/reason, reserved/refundable allocation, provider key/state/compensation links |
| grants/events | customer, promotional amount/expiry, issuer/reason, immutable source events and allocations |
| plan_versions/orders/periods | immutable terms/period policy, paid or complimentary authorization, validity/consumption history |
| reporting facts | rated usage/funding/earned charges/collections/receivables/refunds/cost provenance; unique source references |

Checked integer money, ownership/idempotency uniqueness, composite FKs where helpful, indexed customer/time/job scans. Counters are bounded projections over events, never a second money authority.

## D03. API and capability contract

Change OpenAPI first. CamelCase, standard envelopes, pagination, stable IDs, server dates and idempotent writes. Proposed families below may reuse existing safe endpoints.

- `/api/v1/business`: staff context/profile/setup; `/staff`, `/customers`, `/customers/{id}/members|keys|terms|suspension`.
- `/api/v1/business/channels|models|price-books`: price version publish/schedule and customer assignment.
- `/api/v1/business/customers/{id}/grants|manual-receipts|credit-limit|credit-notes`.
- `/api/v1/business/payment-settings|payments|refunds|disputes|plans|statements|reports|reconciliation|integration-credentials|audit`.
- Existing customer `/api/v1/org/*`, balance/pricing/usage/bills/statements stay compatible where safe; add plan/order/refund-request flows scoped by principal.
- `/v1/*` stays protocol-compatible. Integration credentials may select a customer only within explicit scope. `user`/`endUser` stays a workload label inside that customer, not debit authority.
- Service metering keys stay distinct from consumption keys; new credentials carry explicit scopes.
- Callbacks resolve configured merchant context, verify signature, then use order metadata. Untrusted payload must not select arbitrary merchant secrets.

Maintain a route/capability matrix covering raw entries/proofs/exports, websocket, cache/tasks, account switch and all money APIs. Compatibility does not retain customer self-credit privilege. Enforce authority in core and handlers and audit actor/reason; UI hiding is insufficient.

## D04. Rating, routing and evidence

Resolve `SalePriceSnapshot` separately from route-specific `ProviderCostSnapshot`. Pin version, conditions, discount, rounding, authorized model path, customer and fingerprint at admission. Same-model provider failover preserves sale rates; cross-model fallback reserves the maximum authorized bound or atomically increases the reservation before an attempt, otherwise refuses that hop. Never widen model authority through aliases/fallback/experiments.

Publish canonical sale versions and bind their digests into new settlement descriptions/proofs. A proof of an old channel price cannot prove a different customer override. Extend verifier formats by version; retain every old decoder/fixture. Payload exposes sale terms and funding allocation, not supplier costs/credentials. Proof signer remains deployment/business-controlled, with honest key identity and rotation history. Do not relabel a deployment key as a customer key.

Customer ledger identity and existing `endUser` day logs remain stable. Add whole-customer statement/report evidence where needed without treating a workload label as another payer. Late correction is append-only on the current posting day. Completeness covers its stated log/window, never events the operator omitted before recording.

Cache stays keyed by existing customer organization, channel/model and complete authorization/prompt context. Do not change it to a business-wide cache during ownership refactoring. Version incompatible cache keys and discard stale projections, never migrate cached private responses across customers.

## D05. Transaction and billing-operation boundary

Default: add a narrow transaction-aware Postgres ledger append facility for **one customer's ledger plus oxsum operation/entitlement/outbox state**, not a dual-sale or cross-customer transfer API. Existing `append` retains behavior as a wrapper. Appends keep validation, limits, deterministic IDs, dense sequencing/MMR and rollback semantics. Caller transaction pins the correct ledger namespace; oxsum queries remain schema-qualified. No live stream or external payment call holds a DB transaction.

This extension must follow vendored-engine rules, including an `oxsum change (not upstream):` marker, decision entry and all engine tests. Prototype in T11. If the engine cannot support it safely, document a durable saga with explicit intermediate states and compensations, then pass the same fault tests; do not substitute best-effort projections for money correctness.

Lock order for new coordinated paths: customer admission/control lock → operation row → constraint rows in deterministic key/member/team order → entitlement/grant rows ordered by source ID → ledger append lock. Finalization, expiry and refund paths must use compatible order. Read current engine lock/limit behavior and update conflicting legacy paths together; this is a policy to validate in concurrency tests, not an assumption that existing code already follows it.

Operation states: `prepared → reserved → serving → usage_recorded → settled`; pre-send failure becomes `released`, uncertain external outcome becomes `needs_attention`. Use CAS/version/lease for task ownership. Operation recovery can replay committed state but cannot promise exactly-once external inference. Persist final usage or explicit estimation evidence before money settlement, enough to reconstruct the charge after process death. Heartbeat live streams; stale recovery honors max request lifetime and ownership.

Reservations atomically allocate plan/grant/purchased/credit capacity and persist operation. Settlement atomically releases reservations, consumes entitlements, posts financial effects, commits immutable terminal evidence, and schedules required projections/events. Rebuildable analytics can lag. Day close waits for billing/projection watermark and relevant unresolved operations, with explicit late-adjustment policy. Event/outbox consumers are idempotent.

Fully free or entitlement-funded calls must work with zero wallet funds. Reuse existing net-zero marker idiom (balanced debit/credit of the adjustments account) for a typed non-monetary usage receipt when no financial hold is required. Reservation authority comes from the durable operation/entitlement, not a fabricated 1-minor purchased hold. Update classification, sweeper, proof and rollup logic so markers do not create income or affect balances. Do not relax engine zero-posting validation globally.

## D06. Payment, plan and refund state machines

One Business owns merchant configurations; encrypt secrets and version references. Never put supplier/PSP secrets in proof, logs or browser reads. Preserve historical merchant configuration references through rotation for callbacks/reconciliation.

Payment order purpose is immutable: wallet top-up, bundle, plan period or invoice payment. States include created/provider_pending/paid/fulfilled/expired/failed/needs_attention. A verified paid plan order activates the authorized period; it does not also top up the wallet. Verified invoice payment repays allocated debt; it does not create free credit as well. Duplicate provider/event/order keys yield one effect; same key with different terms is conflict. Payment return URL never credits funds.

Primary baseline connector: Hyperswitch with its Stripe connector for checkout/refund, using existing switch architecture. Keep direct Stripe, Airwallex and x402 features with accurate scope and compatibility; do not require new provider OAuth-import issue #506 for this release. For recurring plans, support renewal orders paid manually through the primary checkout first; existing direct Stripe subscription renewal can be certified separately. UI must state whether renewal is automatic or requires payment.

Plan states: pending_payment → active → renewal_pending/past_due → active or expired; cancel_at_period_end and scheduled_change are explicit. Period issuance requires unique authorization `(customer, subscription, period, payment/grant)`; timer rollover alone cannot sell a free paid period. No grant of refundable money for included quota. Pin old period allocations through boundary/expiry and release unused reservations according to expiry policy.

Refund states: requested → reserved → submitted → pending → succeeded, or failed/released/needs_attention. Refundable amount is bounded against source payment less succeeded and pending refunds. A refund request reserves unspent refundable funds; an in-flight refund blocks conflicting consumption. On provider success, post money-out once; on definite failure release reservation; ambiguous provider timeout keeps it reserved pending reconciliation. A spent balance cannot become silently negative: require a linked sale credit note/approved correction or reject excess. Never mark a local liability reversal as a completed provider refund.

Credit notes correct usage or plan documents; returned purchased value, restored promotional entitlement, and debt reduction follow original funding sources. Promotions are never cash-refunded. Disputes freeze/flag under explicit policy and do not automatically erase historical charges. Manual bank receipts require unique reference, amount/currency, authorized finance actor and reason; reserve grants for non-cash promotions.

## D07. Revenue and reporting policy

Keep ledger balance changes authoritative and financial events source-linked. Show separately: rated request value, included entitlement, promotional share, purchased-credit charge, credit draw, subscription charge, payment received, refunds and remaining receivable. Do not sum `UsageRow.charged_minor` as new cash/revenue under plans.

Usage revenue is the actually chargeable purchased/receivable portion under sold terms; show promotional list value as promotional usage, not paid revenue. Prepaid money-in is funding, not earned usage. For paid recurring service, record fee into deferred plan revenue and recognize linearly over the sold UTC service period; integer remainder lands on final day. Use an additive bookkeeping account/event with corresponding ledger facts, never synthetic customer spend. Cancel-at-end retains recognition through the paid end; an explicit refund/credit note reverses unearned liability first and earned revenue only as specified, without changing prior entries. Reports label this operational recognition policy, not a universal statutory tax treatment.

T20 must document and test the journal map for plan fees/recognition/corrections and reuse existing per-customer ledger using additive accounts. Legacy usage/pools remain decodable. Report bookings, service periods and payment timestamps distinctly. Provider expense is estimated per attempt when known; reconcile provider/day invoices at supplied granularity. Unknown differs from zero; allocated cost is labeled. A cache hit does not incur a new inference expense merely because its usage was copied. Failed/retried attempts may incur expense even when customer charge is zero; retain missing-cost coverage.

## D08. Migration, setup and compatibility

Add default Business and customer mappings idempotently, preserving organization IDs and ledger pointers. Do not automatically appoint an existing customer's owner as business owner. Authenticated deployment administrator explicitly appoints the initial business owner; initialization shows preview/counts and records a migration receipt. Global channels/plans acquire default ownership without rewriting price history.

Safe compatibility: gateway keys and customer read paths continue; unsafe self-credit/administration privileges are removed with release notes, not a permanent compatibility bypass. Fresh and upgraded installations default invite-only until owner selects registration policy. Never publish demo credentials into production. Production seed requires explicit demo profile/isolation.

Old `.env` merchant settings may serve as a clearly labeled default-business bootstrap adapter; don't print or copy secret plaintext to planning files. New settings use encrypted storage and capability checks. Existing currency/display settings never rewrite balances or payment conversion.

Upgrade testing compares pre/post IDs, amounts, hashes, roots, settled proofs, statements and key resolution on a captured fixture. Restore tests include encryption/signing keys and job leases; do not assume a DB-only restore suffices. Document forward migration and rollback compatibility, backing up before irreversible changes.

## D09. UI, deployment and performance

Use existing React design primitives/locales. Business and Customer navigation contexts are explicit; customer pages reuse existing dashboard where correct. Operator can test real route/price/payment configuration with clear billable actions; no hidden paid probe. Provide empty/loading/denied/error/retry and keyboard/mobile states. Browser verification exercises the built artifact, not only source-level tests.

Core deployment remains binary/container plus PostgreSQL. Payment switch profile documents its own PostgreSQL/Redis/services; direct-rail profiles accurately state dependencies. Publish pinned versions/digests; upgrades never auto-run a live production deploy from an ordinary feature merge.

Reference capacity target for measurement: 1,000 customer accounts, 10,000 keys, 100 concurrent stub streams, at least 20 newly admitted/settled requests per second, p95 gateway overhead under 100 ms on declared 4-vCPU/8-GiB test infrastructure, no unexplained drift. These are targets, not current claims. If actual hardware differs, publish reproducible normalized workload/results and fix correctness failures; do not invent measurements. A documented capacity shortfall is a release limitation or follow-up, not silently lower acceptance. Backpressure must be explicit, never duplicate/missing money. Persist measured results in T27/T30.

## D10. Journal and allocation acceptance map

Preserve legacy wallet accounts/credit-line mechanism rather than renaming old postings. Record final additive account paths and the following economic treatment in accounting fixtures.

| Event | Treatment |
| --- | --- |
| Purchased-credit payment | Existing cash debit / purchased-wallet liability credit; pending payment grants nothing |
| Funded usage | Release pending reservation and charge original source; record exact paid/promo/credit classification |
| Promotional usage | Preserve pool behavior; exclude promotional face value from paid net usage revenue, reconciling any gross income posting to its promotional contra/classification |
| Credit usage/payment | Retain facility/draw semantics and derived debt; payment repays debt before new purchased availability under explicit allocation |
| Paid plan activation | Cash or settled clearing debit / deferred-plan revenue credit; entitlement opens under same durable authorization, not wallet top-up |
| Daily plan recognition | Deferred-plan debit / subscription-income credit; unique period/day entry and exact final remainder |
| Wallet cash refund | Reserve refundable purchased value; on confirmed success debit wallet liability / credit cash or refund clearing once |
| Plan refund | Reduce unearned deferred amount first and earned income only through approved correction, with corresponding cash/refund clearing; never pay again for an already credited note |
| Sale credit to wallet | Reverse original paid-revenue/debt share into purchased liability or debt relief; return promo/entitlement only to eligible original non-cash source |

A credit note can create a later refundable liability. Link sources so one fee cannot be returned both directly and through wallet credit. Partial refund conversion uses the original pinned rational basis and deterministic remainder allocation, bounded in both payment and credit units. Repeated rounding cannot exceed the original. Fees/FX estimates are separate, never silently deducted from customer allowance.

Each fixture states opening balances, action, exact postings/events, available/held/debt/entitlement and revenue/collection totals. If legacy account labels differ from economic reporting, disclose the reconciliation bridge without rewriting history.
