# Operator launch execution contract

Accepted: 2026-11-08 (Asia/Shanghai). Tracking issue: [#725](https://github.com/AllureCurtain/oxsum/issues/725).

This is the adopted N00–N21 product/software and acceptance scope, transcribed from the November 7 launch plan and resolved by the November 8 execution brief and user instruction. It supplements the existing [self-hosted product contract](../self-hosted-v1/PRODUCT.md), preserving its original release gates. Current progress, issue ownership, exact evidence and external prerequisites live in [EXECUTION.md](EXECUTION.md). Names R01–R14 and J01–J18 in this directory refer to this stage; the earlier roadmap retains its own IDs.

## Execution boundaries

Implementation, isolated environments, test fixtures, issue/PR creation, branch pushes and normal protected merges after applicable checks/review are authorized. Every minimal feature follows issue-before-code and a fresh worktree from current main. Existing architecture, ledger identities/history, immutable prices and proof compatibility are preserved. Frontend changes use existing components and both locales.

The first operator is fictional and invite-only, with prepaid pay-as-you-go, one model, direct Stripe as the payment rail and local captured mail. New isolated fixtures use USD and rational 1 credit = USD 1. Plans, teams, postpaid, refunds, disputes, daily recognition, evidence and embedded integration remain required. Staff invitations expire in seven days. Mutable business settings must not reinterpret funded history.

DeepSeek has one run-wide CNY 0.10 ceiling, at most five upstream attempts (default two), at most 32 output tokens per attempt, short bounded inputs and thinking explicitly disabled. Reserve cost and attempts durably before sending; timeouts/retries count. Credential stays in ignored root `.env`. Regression, fault, concurrency and soak tests use local fixtures. No automatic recharge or other paid service.

The discarded Hyperswitch test stack may be rebuilt with pinned components, task-owned persistent volumes and fresh merchant keys — **but it is optional, not a launch blocker**: the active payment rail is direct Stripe (live-verified #1047 + the stripe-sandbox evidence), and Hyperswitch's only distinct value is multi-PSP orchestration a single-processor operator does not need. Rebuilding it is deferred until a second PSP is required; a genuine PSP sandbox credential for it is absent regardless. Local captured mail is authorized; external contact is not.

No live payments, paid cloud provisioning, public package/image/release publication or existing remote-service upgrade is authorized in this run. Prepare and rehearse their tools in isolation. Independent pilot results require actual external participants; none have been supplied. Native Linux amd64 4-vCPU/8-GiB reference performance and eight-hour soak thresholds remain unchanged; emulation and resource competition must be identified.

Software completion, candidate validation, external certification and public launch are distinct verdicts. Missing external inputs never excuse an implementable defect or stop independent work.

## Accepted product decisions

### 3.1 Established constraints

- One operating Business per deployment; many individual/team customer accounts. Existing Organizations remain customer billing accounts.
- The operator owns upstream credentials, sale prices and collection. The oxsum project receives no payment or revenue share in this workflow.
- Keep Rust, axum, PostgreSQL 17, the vendored doubleentry engine, React/Vite and the independent verifier.
- Preserve IDs, balances, ledger origins, immutable price history, recorded funding and old proof compatibility. No historical reinterpretation to make new reports look consistent.
- Retain prepaid, promotional, plan, refund/dispute and controlled-postpaid capabilities from the accepted contract. A simpler first journey does not remove them from required validation.
- Money uses checked integer minor units and explicit rational conversion. Customer rate, provider cost, funds received, entitlement and recognized revenue remain separate facts.
- Production documentation/comments/commits are English; preserve the existing English/Chinese UI resources and test both.
- Follow issue-before-code, one minimal feature per PR, task worktrees, contract-first APIs and current repository gates. Never bypass branch protection.

### Accepted defaults

| ID | Decision | Accepted default | Effect if changed |
| --- | --- | --- | --- |
| P01 | First audience | Businesses selling AI service: API operators and AI application/Agent SaaS builders, including teams delivering such services for clients. Both the built-in portal and one complete embedded integration are required | Build the portal journey first, then N21; this is sequencing, not exclusion of the embedded product |
| P02 | Product promise | “Self-hosted AI-service billing and operations, with customer-verifiable bills” | Portfolio engineering remains a supporting explanation, not the operator onboarding path |
| P03 | First journey | Single owner, invite-only customers, pay-as-you-go, one configured model and one primary payment connector | Plans, extra staff and open registration are optional for initial readiness, but their advertised workflows still require acceptance |
| P04 | Plan revenue policy | Restore the accepted daily service-period recognition requirement for newly versioned policy; preserve historical postings and legacy policy explicitly | N11 implements and tests daily recognition. Invoice issue timing, cash receipt and customer proration remain separate decisions; this supersedes this draft's earlier period-close recommendation |
| P05 | Payment route | **Direct Stripe** is the active primary route (the only rail live-verified end-to-end: capture→webhook→credited + refund recovery #1047). Hyperswitch stays an optional orchestration layer, deferred until multi-PSP is needed; it is env-gated and adds a self-hosted switch | Selecting another primary route requires its equivalent capture/refund lifecycle and environment; no silent switch after credentials fail |
| P06 | Renewal | Explicit customer-paid renewal orders first; label them as manual renewal | Automatic provider subscriptions remain separately certified; no timer grants a paid period |
| P07 | Distribution/platform | Validate a pinned Linux amd64 container plus PostgreSQL; keep other architectures accurately labeled | Advertising arm64 requires native runtime/restore/browser evidence and publication of the corresponding candidate |
| P08 | Browser setup | Operator mints a one-time setup token; intended owner accepts through a browser. All subsequent ordinary business setup uses staff sessions/capabilities | Requiring a fully browser-only infrastructure install would add deployment/control-plane scope |
| P09 | Verification experience | First-turn proof immediately; closed-period evidence later; independent packaged verifier with explicit trust inputs | No backdating to manufacture a closed-period report on day one |
| P10 | Target completion | Complete the product through both adoption paths and pursue launch readiness; RC is an intermediate checkpoint. Real-money/pilot/publication actions need their concrete entries in the execution authority record | Missing external inputs leave their tasks pending; they do not shrink software scope or grant permission to spend/contact/publish |
| P11 | Capacity follow-up | Retain D09 thresholds; add a controlled reference run, an 8-hour steady workload and an isolated restore drill | A lower preview envelope may be proposed with evidence, but accepting it is an explicit scope decision |

P01–P11 were accepted on November 8. Original D07/D09/D10 requirements retain their meaning. External evidence and action limits are recorded in [EXECUTION.md](EXECUTION.md).

### 3.3 Product shape and boundaries

The primary story is: an operator installs oxsum, configures supply and customer sale terms, invites a customer, receives payment, serves a metered request, explains the charge, and completes refund or period-close operations when needed. The customer can retain and independently inspect the evidence.

Daily business actions must work under a business session. A deployment token remains suitable for initial setup and documented recovery, not the normal credential for finance/support staff. Customers never need supplier credentials or internal UUIDs to understand their bills.

Keep the current gateway protocols and correctly describe each tested provider/parameter combination. New provider OAuth import (#506), broad media support, automatic quality routing, a seller marketplace, multi-currency wallets, tax invoicing, proration and multi-region operation are not default additions to this stage. Existing capabilities remain available with truthful support levels.

### 3.4 Recommended audience and adoption paths

The buyer/installer is a business that needs to charge its own customers for AI service. Its owner/admin, finance and support staff use the operating console; its customers use either the oxsum portal or that business's existing application. These are different roles in one commercial relationship, not unrelated product markets.

| Audience | Concrete example | Why the existing system fits | Required completion |
| --- | --- | --- | --- |
| API-service operator | A small team sells access to several model endpoints to individual and team customers | Provider pools/routing, customer sale books, wallet/limits, plans, checkout, refunds and proofs already share a domain model | The built-in portal/console supports the whole operating cycle, including day-two changes and recovery |
| AI application or Agent SaaS builder | A document assistant or research workflow charges its own users for model calls or completed billable jobs | Gateway plus external metering, customer attribution, idempotent operations, entitlement and evidence are reusable | N21 supplies bounded lifecycle APIs and one runnable application example; account creation/keys/checkout/reads cannot depend on owner cookies |
| AI delivery/integration team | A team installs a service for a client and later hands operation to that client's staff | Self-hosting, separate staff/customer roles, versioned artifacts and restore procedures match handover needs | Reuse the two paths above and demonstrate owner/staff handover and documented maintenance; no new reseller hierarchy |
| Internal AI platform | A company assigns usage and limits to teams | Team keys, caps, cost attribution and verification already help | Keep a documented internal allocation path without presenting internal transfers as external sales; broad enterprise IT/compliance features are not a prerequisite for this launch |

The first two independent pilots should exercise different adoption paths: one built-in-portal operator and one application integrator. The integration team is a useful acquisition channel for the same software rather than another technical stack. This is a source-based product recommendation; demand and willingness to adopt still require pilot evidence.

### 3.5 What completeness means for this stage

Completeness includes acquisition/setup, first value, daily operation, exception handling, period close and upgrade/restore. Specifically:

- An operator can configure and change its business without using a database console or a deployment token for ordinary work.
- A customer can purchase/use/cancel/refund and understand the financial outcome, including errors and delayed external outcomes.
- An application developer can connect the same account, entitlement, collection and evidence lifecycle through narrowly scoped machine APIs.
- Staff can reconcile amounts and resolve exceptions from source-linked records, with an explicit recognition policy.
- The supported release is reproducible, measured, restorable and backed by actual external integration evidence.

Passing one prepaid demo does not close the stage. Existing plan/postpaid/funding/proof capabilities remain in scope. Additional modalities and enterprise infrastructure are admitted by a concrete supported workflow, not by competitor feature counts.

## 4. Requirement map

| Requirement | User outcome | Work units | Acceptance |
| --- | --- | --- | --- |
| R01 Positioning and first run | A new visitor understands the product and can start the documented release | N01–N03 | J01 |
| R02 Owner and staff setup | The intended owner initializes the business and enrolls staff with consent | N03,N08,N09 | J02,J07 |
| R03 Supply | Owner/admin manages supported models, credentials and routing with appropriate authority | N04 | J03 |
| R04 Sale terms | Staff publishes and assigns customer prices, previews them and preserves admitted versions | N05 | J04 |
| R05 Collection | Owner configures the primary merchant; customers pay through it; finance can resolve refunds | N06,N10,N15 | J05,J09 |
| R06 Customer admission | Operator invites customers or deliberately opens registration; teams remain isolated | N07 | J06 |
| R07 Readiness | Solo operators and fully entitled customers receive accurate, actionable guidance | N09 | J02,J08 |
| R08 Commercial cycles | Prepaid, grants, plans, postpaid, disputes and corrections finish through their intended roles | N10,N11 | J09–J12 |
| R09 Evidence | A real customer export works outside the served page and rejects relevant tampering | N12,N18 | J13 |
| R10 Distribution | The artifact accepted is the artifact published and deployed | N13,N18,N20 | J14 |
| R11 Real integrations | Claims name executed provider/PSP scenarios and exact external limitations | N14,N15 | J05,J08,J09 |
| R12 Operation | Capacity, jobs, upgrade/restore and diagnostics have reproducible evidence | N16,N17 | J15,J16 |
| R13 Adoption | Two unfamiliar operators independently complete installation and a billing/plan cycle | N19 | J17 |
| R14 Embedded adoption | An external application manages its own customers' allowed billing lifecycle using scoped credentials | N21 | J18 |

Adoption does not complete implementation or acceptance. Current N-unit status is recorded in [EXECUTION.md](EXECUTION.md). A work unit with several independently reviewable changes splits into child issues; it is not permission for a single large PR.

## 5. Work units and completion criteria

### N00 — Rebaseline and adopt the execution contract

Baseline dependency satisfied: #697 closed and #724 merged at `0314cbd3d5439adbc904a327a8e16704b344a0af`.

- Inspect actual main/worktrees, current issues/PRs, CI commands and the current contract. Reconcile every source finding above; reuse any equivalent change already integrated.
- Create an umbrella issue and focused child issues with acceptance before code. Record accepted P decisions in tracked product/design/decision documents; link work to #622 where it closes an external gate.
- Create tracked status/evidence under a dedicated next-stage directory, `docs/roadmap/operator-launch/`. TODO contains issue status and links, not another duplicated specification.
- Completion: every required R/J row has an issue owner in the task graph; environment entries are ready or explicitly pending; no task relies on an assumed credential or publication permission.

### N01 — Align current product claims and documentation

Depends on: N00.

- Align README, product page, product/user/developer guides, AGENTS' project description, support/verification documents and release status with the current Business/customer model.
- Separate advertised capability, software test coverage, provider-sandbox evidence, live evidence and known limitations. Reconcile rails/protocol matrices and the current T31 status with executed evidence.
- Describe statement/usage/entry verification individually: checking a report does not automatically mean every surface performs every arithmetic/completeness check. Explain retained keys/heads and verifier distribution.
- Completion: one recommended operator entry point; no current instructions claiming customer self-credit, env-only signup, universally certified providers, an independent third-party witness merely because it is a second process, or a published package without evidence.

### N02 — Repair the runnable demo and first-run example

Depends on: N00; align final docs with N01/N03.

- Replace old `demo/demo.py` registration/top-up/balance assumptions with current owner/customer session and receipt/checkout authority. Reuse maintained examples where practical; avoid two divergent bootstrap flows.
- Run on an isolated empty database with a deterministic upstream and explicitly labeled local funding. Never fall back to a user's ordinary database when scratch provisioning fails.
- Produce a real settled entry, retained verification inputs and a passing independent check; altered evidence fails. The script exits nonzero on any failed or skipped essential step and cleans only its own resources.
- Completion: the exact README command works from a clean checkout with documented prerequisites; it requires neither an external paid model nor a production secret.

### N03 — Owner setup, business profile and operating settings

Depends on: N00. Suggested children: owner setup; profile/settings.

- Expose existing one-time setup acceptance/registration in the browser. Handle new and existing identities, bound email, expiry, replay and completed setup. Never make the first public registrant an owner.
- The deployer may mint the token with the documented CLI/API. Subsequent setup uses the owner's session, explicit business context and existing second-factor rules.
- Provide a coherent business display name/contact/help identity. Accepted source rule: persist mutable business profile in the business record; environment branding supplies bootstrap/default values, with explicit migration and precedence. Historic documents retain their original snapshots.
- Surface settlement currency and credit conversion before collection. Reuse existing configuration semantics; block relabeling funded currencies and pin historical conversions. Expose who can change what; new permission/schema fields are contract-first.
- Completion: fresh initialization, reload, expired token, attempted takeover and pre-existing-customer cases pass; visible branding/help and operational currency match their authoritative configuration. A styling framework replacement is unnecessary.

### N04 — Business provider/channel administration

Depends on: N03.

- Implement business-authorized channel creation/update, write-only credential add/rotation/disable/remove, provider billing credential, model/alias administration and existing supported route/fallback/cache settings.
- Reuse existing business APIs/core capabilities rather than exposing the deployment token. Respect stronger owner authority for secrets and internal destinations.
- Distinguish provider cost from customer sale terms. A connectivity HEAD check is labeled reachability, not successful inference or billing certification. Saving configuration never silently makes a billable call.
- Provide explicit request tests with visible cost consequences and established execution budgets; do not weaken DNS/IP/redirect policy to make a test pass.
- Completion: configure one provider entirely through the business UI, reload it, rotate a secret without readback, create a working route and observe an unauthorized staff denial. Requests admitted before a change retain their pinned context.

### N05 — Sale-book creation, publication and assignment

Depends on: N04. Suggested children: editor/publication; schedules/customer terms.

- Implement the existing `/business/sale-books/changes` operations: create, save with `expectedRevision`, publish an exact revision, schedule/replace/end a default, assign a customer book and manage a customer override.
- Explain that publishing a version does not activate a schedule. Display effective UTC boundaries, current resolution source and customer preview using existing deterministic precedence.
- Use exact decimal-to-integer/rational conversion and bounded input validation; never turn monetary text through floating-point multiplication. Apply the same rule when correcting touched existing money forms.
- Preserve drafts on validation/network errors. Keep one idempotency key for retries of the same unchanged intent; changed payloads use an explicit new action. Handle concurrent edits and non-overlapping schedules.
- Completion: a new operator publishes an active default; two customers get different intentional terms; preview, admitted charge and exported sale proof agree; a mid-stream edit cannot reprice an admitted turn.

### N06 — Merchant configuration and payment readiness

Depends on: N03; primary route selected in P05.

- Add create/version/enable/disable flows for the existing merchant model, with test/live mode, identity, write-only credentials, published callback coordinates and rail-specific fields.
- Fully guide the chosen primary connector; retain accurate status for other existing rails. No implicit conversion of test configuration into live operation.
- Distinguish configured, reachable, checkout available and externally certified. A merchant row alone never means payments are working. Retained credential versions must still process legitimate late callbacks.
- Pin and document the supported payment-rail components/SDK versions for whichever rail is active. For Hyperswitch specifically (now optional), pin its components/migrations/SDK versions only when it is actually deployed; resolve public HTTPS SDK/callback requirements with a declared test topology; do not disable URL or destination validation for convenience.
- Apply the configuration-audit findings recorded in [EXECUTION.md](EXECUTION.md): the user confirmed lost disposable test data and selected a fresh Docker switch rebuild. Generate new merchant credentials, correct the fixture-versus-Stripe setup guidance, verify switch database persistence, and configure the SDK backend origin as well as the script origin. Old-data recovery is no longer a prerequisite.
- Completion: owner configures the primary merchant through the UI, a customer obtains checkout, a local signed-event fixture credits once, finance/support cannot read secrets, and invalid/rotated/disabled configurations have usable recovery states. Real payment completion belongs to N15.

### N07 — Customer invitation and registration controls

Depends on: N03.

- Add business customer-invitation create/list/revoke/expiry UI and owner-controlled open/invite-only registration. Where safe list/delivery APIs are absent, add them contract-first.
- Support copy-link and configured-mail delivery to the explicitly selected recipient. Bind customer external identity and intended email, distinguish new/existing-user acceptance and retain consent on retries.
- Show mail readiness and delivery failure without claiming receipt. Outbound delivery must not duplicate on replay. Use authorized test mailboxes for acceptance.
- Preserve customer-team invitations as a distinct membership operation. Customer registration/acceptance never grants staff or free purchased funds.
- Completion: an invite-only empty deployment enrolls its first customer without manual API preparation; wrong-email, expired/revoked/replayed invitations and existing-user cases behave correctly; a deliberate open-registration change survives reload.

### N08 — Staff invitations and role lifecycle

Depends on: N03. This requires a bounded API/domain extension where only UUID appointment exists.

- Add email-bound expiring staff invitations with explicit recipient acceptance. Accepted default: seven-day lifetime, at most the role the inviter may grant, no owner transfer through an invitation, no secret stored in plaintext.
- Owner manages roles/revocation through named identities. Reuse consent, password and TOTP primitives without inheriting customer funds or account access.
- Keep last-owner/ownership-transfer rules and current-role checks. If an existing appointment endpoint remains for compatibility, it must retain its original restricted authority and audit.
- Completion: owner invites finance/support, each accepts through their own session, denial boundaries hold, revocation removes authority, replay cannot restore revoked membership, and normal onboarding never asks for an internal UUID.

### N09 — Journey-specific readiness and first bill

Depends on: N03–N08; N10 supplies entitlement/payment outcomes.

- Separate business readiness from customer getting-started. Read authoritative facts under the actual actor; stop requiring a deployment token in the customer's browser.
- Business basics: initialized profile, a permitted/priced serving route, active customer sale terms, selected funding/collection readiness and an available customer admission path. Optional staff/plans do not prevent solo/pay-as-you-go readiness.
- Customer basics: available model/terms, permitted key, sufficient authorized funding or paid entitlement, successful call and available evidence. Zero purchased balance can still be ready for fully entitled/free usage.
- A first-turn proof is immediately accessible. Period exports explain the closed-day prerequisite and do not imply every account must mint an end-user report link to be ready.
- Completion: solo owner, invited customer, team member, prepaid and fully entitled zero-wallet paths show correct next actions; inaccessible facts are not reported as complete or endlessly loading.

### N10 — Complete commercial journeys and targeted repairs

Depends on: N05–N09; N11 decides financial interpretation. Split repairs by actual workflow, not a general cleanup PR.

- Prepaid/bundles/promotions: price display, payment purpose, purchased versus promotional portions, expiry, concurrent use and refund eligibility are understandable and conserved.
- Plans: publish/bind/buy, paid activation, included quota, overage opt-in, manual renewal, failure, next-boundary change/cancel and expiry. Show calendar versus anniversary policy as the sold version actually defines it; no hidden automatic renewal claim.
- Postpaid: approved credit, statement finalization, exact debt, partial/full payment allocation, overdue suspension/restoration and correction. Existing paid quota and manual suspension keep their documented independent semantics.
- Refunds/disputes: customer request, finance approval/decline, reserved amount, provider-pending/failed/succeeded states, linked credit notes and unknown-outcome recovery. A local reversal does not claim money returned.
- Reports: drill from each displayed total to requests/orders/journals/entries and its adjustment. Show unknown or allocated costs distinctly from zero cost and observed per-customer costs.
- Completion: J09–J12 pass through customer/finance/support browser roles plus meaningful domain concurrency/recovery tests. Setup via API is permitted in focused tests, but cannot replace J02's fresh-operator acceptance.

### N11 — Implement the agreed revenue policy and prove reconciliation

Depends on: P04, N00; precedes final N10 evidence.

- Implement P04's daily recognition for a newly versioned plan accounting policy and make daily/period report semantics explicit. Preserve old entries, closed journals and active legacy policy; choose and record a compatible next-period activation boundary instead of rewriting history.
- Reuse the existing ledger and durable jobs. Book each completed UTC service-day's earned share under a unique period/day effect; calculate the cumulative earned amount with checked integer/rational arithmetic and allocate the exact final remainder at period close. The closing journal must not recognize the same fee twice. Partial first/last days, month lengths and refunds require exact fixtures.
- Recognizing delivered service each day does not introduce customer upgrade proration, alter the sold fee or mean the PSP charges every day. Those are separate commercial actions. This policy corrects the original design/implementation discrepancy; it is not a claim of statutory accounting compliance.
- Trace prepaid fees, earned revenue, receivables, purchased balances, quota, promotions, corrections, refunds and provider costs from exact source events. Check reported totals against postings and across customers.
- Cover mixed funding, partly spent refund, mid-period correction, partial payment, boundary rounding and unknown supplier cost. Preserve originals; repair detected defects with targeted issues.
- Completion: reviewers can reproduce representative opening/action/closing amount tables and verify conservation without treating quota as cash or collection as earned revenue. A business-policy choice and a correctness defect remain distinct.

### N12 — Independent evidence delivery and useful verification

Depends on: N09–N11.

- Make download, expected hash/key retention and independent verification discoverable from an actual request/statement. Reuse the WASM SDK, evidence formats and packed-consumer example.
- Distinguish a supplied operator key from an independently trusted key; distinguish a second local witness process from an organizationally independent witness. Preserve honest unsigned/unanchored/missing-witness states.
- Verify a real export from the candidate with the packed SDK outside the workspace and with network access disabled. Check the expected rejection matrix for each format, including changed amount/rate, missing committed artifact, wrong origin/customer/key and v1/v2 compatibility.
- Completion: a customer can retain evidence and reproduce the checks after the original link expires. Package publication is N20; packaging does not require publishing.

### N13 — Promote the validated artifact and deploy by identity

Depends on: N00; can proceed independently of UI work.

- Require a candidate manifest identifying the exact source SHA, successful full CI including heavy/engine checks, image platform/identity, SDK artifact and checksums. Reject missing, skipped, wrong-SHA or expired/unavailable evidence.
- Promote the validated image archive to the authorized registry instead of rebuilding a moving ref. Record registry digest and image/config identity correctly; a tar checksum is not an OCI manifest digest. If an artifact must be rebuilt, treat it as a new artifact and validate it before promotion.
- Publish SDK bytes from the verified tarball. Keep release-note preparation separate from actual publication. Prevent accidental overwrite of an existing version with different bytes.
- Deploy an explicit digest/artifact; check readiness and reported SHA. Separate deployment/rollback from the build/publish workflow. A rollback selects an existing image and follows database compatibility rules; never invent a safe down-migration.
- Support the same pinned image reference in Compose for serving and migration. Remove the need for a `latest` default on the accepted release path; preserve documented compatibility where needed.
- Completion: negative gate tests reject mismatched candidates; an isolated promotion/deploy rehearsal lands the expected image/version; rollback rehearsal uses a compatible retained artifact. Public push and live server restart still follow the execution authority record.

### N14 — Real-provider certification for the selected matrix

Depends on: N04,N05,N12 and authorized provider entries/budgets.

- Define exact provider/model/protocol/SDK versions and required parameters. First certify the primary OpenAI-compatible chat whole/SSE path. Add Responses, Anthropic, Gemini, embeddings and rerank rows for providers the release intends to certify; a generic compatible endpoint is not universal support.
- Begin with the existing DeepSeek account and the tiny smoke contract in the configuration audit recorded in [EXECUTION.md](EXECUTION.md): two short calls, at most five upstream attempts, 32 output tokens each, thinking disabled, authorized CNY 0.10 total ceiling. Do not buy accounts or run paid load tests to complete local software coverage. Other real certification rows require available accounts and their own concrete scope.
- Update the opt-in real-provider harness's legacy registration/top-up setup to current authority, make the model-specific nonthinking parameters explicit, and bound attempts/input/output before running it. Its current short requests do not by themselves establish compatibility with the new setup path.
- Exercise successful usage, client cancellation, missing-usage policy where reproducible, price/funding evidence and supported tools/cache/reasoning dimensions where applicable.
- Verify fallback/retry against real reachable providers within the budget. Use explicitly labeled local fault injection for reproducible refusal/timing conditions; do not provoke provider outages or label injected faults as provider observations.
- Charge and attempt ceilings apply to retries and probes as well as successful calls. Provider 401/403 is an unavailable credential result, not permission to rotate through arbitrary accounts.
- Completion: every certified row has exact candidate/runtime, request scenario, actual usage/charge, retained evidence and limitations. Unfunded/unselected rows remain software-tested or pending, never silently “certified.”

### N15 — Primary PSP sandbox and bounded live certification

Depends on: N06,N10,N12, valid merchant environment and authority record.

- Run the PSP's actual test checkout/capture, real provider-delivered authenticated event, fulfillment, partial/full refund where supported and provider confirmation. Reconcile PSP reference, order, refund, credit note and ledger.
- Exercise cancellation/decline, duplicate/reordered delivery and recovery after an interrupted worker. Synthetic event replays remain labeled as such; at least the main success/refund path must originate at the PSP.
- Verify renewal-order and invoice-repayment purpose isolation against the primary connector. Test dispute handling in the provider sandbox if supported; otherwise retain local evidence and the stated external limit.
- A separately authorized low-value live exercise uses the named payer/recipient, currency, transaction count and gross amount ceiling. Refunds can cost fees and do not reset the spending budget.
- Completion: sandbox and live verdicts are separate. No manually inserted provider reference or locally signed success closes a real-payment gate.

### N16 — Capacity, fairness and long-duration operation

Depends on: stable N04–N12 implementation; isolated E4 environment.

- Reproduce D09 on declared 4-vCPU/8-GiB Linux infrastructure with 1,000 customer accounts, 10,000 keys and 100 concurrent streaming slots. Retain the >=20/s and <100 ms p95 gateway-overhead targets.
- Report offered/admitted/completed throughput, concurrency, measured percentiles, client/load-generator constraints and a direct-upstream comparison. The old 100-slot/five-second shape has almost no throughput headroom; diagnose that measurement constraint without silently changing the target or replacing its results.
- Run the real background workers. Profile lock waits, database pool saturation, schema migration/opening, hot-account fairness, settlement/proof costs and recovery before selecting optimizations.
- Required soak: 8 hours at 10 offered calls/s, five-second stub streams, up to 100 slots across the same account/key population. Record memory, storage growth, job lag and hourly conservation; stop on corruption/unexplained drift and preserve evidence. These are test settings, not an existing capacity claim.
- Completion: zero unexplained per-customer drift and leaked reservations; targets either achieved on the specified environment or explicitly still failed. A narrower preview requires an explicit recorded decision, never a relabeled pass.

### N17 — Operator diagnostics, upgrade and restore

Depends on: N13; run final checks after N16 fixes.

- Make startup/SMTP/provider/merchant/signing readiness diagnosable without secret exposure. Provide a documented sanitized report of version, schema, health, recovery queues and configuration completeness.
- Exercise old-supported-version to candidate upgrade with preserved balances/proofs/configuration. Include active-operation drain/recovery and credential/signing-key retention.
- Back up and restore the representative multi-customer dataset to a separate environment; measure dump/restore/migration duration, disk requirements and PostgreSQL lock/shared-memory settings. Set realistic operational recommendations from results.
- Document routine job/settlement/payment recovery and caller-visible backpressure. Verify a restored deployment can serve a new call and verify old evidence.
- Completion: demonstrated restore and compatible rollback, known data-loss window/restore time for the test setup, no production destructive rehearsal, and no unmeasured uptime guarantee.

### N18 — Final integrated candidate acceptance

Depends on: N01–N17 and N21 to the selected external scope.

- Freeze a merged source SHA and run the required full CI and candidate artifact workflow. Include executed process-shared and engine suites, frontend/WASM, clean verifier consumer and the portal/embedded example checks.
- Run J01–J16 and J18 against the actual built candidate image and its frontend. The full fresh-install journey uses the UI after documented setup-token creation; a pre-provisioned fixture is recorded as a different test.
- Record exact evidence for test tiers. Any code change invalidates the candidate identity and requires relevant revalidation plus final full gates; document-only evidence commits do not silently change which binary was tested.
- Completion: a reviewable manifest, image archive/identity, SDK tarball, checksums, sanitized workflow evidence, release notes and limitations. Candidate readiness, external certification and publication remain separate verdicts.

### N19 — Two independent operator pilots

Depends on: N18 and two actual participants with an available installation environment.

- Give each participant the same identified package and instructions. Select one built-in-portal operator and one application integrator. Record install-to-first-bill time, author assistance, failure points, daily operations, refund/statement comprehension and willingness to continue.
- Each completes installation plus a billing/plan cycle. Any shortened-cycle technique must use a documented isolated test clock/environment and be declared; never alter a live ledger's historical dates.
- A pilot using the paid path requires its own named external accounts and budget. If using manual receipts instead, label that pilot scope and retain the separate PSP certification requirement.
- Completion: two distinct external operators' recorded results, artifact/environment identity and actionable defects. Internal agents, seeded fixtures and author-operated walkthroughs do not substitute for participants.

### N20 — Publication and deployment, if selected

Depends on: N18, applicable N19/live gates and recorded action-specific authority.

- Separate minimal actions: versioned release/image publication; npm publication of the verified package; isolated public demo update; any existing live deployment/real-data upgrade.
- Bind each action to the approved candidate, target, version, registry account and migration/rollback plan. Do not deploy an old default live host merely because its workflow exists.
- Complete smoke/readiness/version/verification after the authorized action; retain resulting URLs, immutable digests and observed state.
- Completion: publication and deployment are observed facts, not inferred from a build. Unselected or unauthorised actions remain visibly unperformed.

### N21 — Complete embedded application integration

Depends on: N03,N05–N07,N10–N12. Required in the accepted completeness scope; replaces the earlier conditional C01 suggestion. Suggested children: account/grant model; lifecycle APIs; external application walkthrough.

- Define two explicit customer entry paths using the existing customer ledger identity: portal customers enroll by personal consent; an application may provision its own service-managed billing account under a dedicated grant without fabricating a human login or staff membership. Later linking to a person requires an explicit verified consent flow. Do not weaken the existing last-owner or invitation rules to create the second path.
- Provide stable business/application/external-customer identity mapping and idempotent provision/retrieve/archive semantics. Customer isolation must apply to keys, funds, history, evidence, requests, caches and task/replay state.
- Owner issues a credential with only the scopes needed for the integration: bounded provisioning, key lifecycle, entitlement/allowed-model reads, checkout initiation and customer billing/evidence reads as actually required. Select exact scope names contract-first. Provisioning authority is confined to an explicit application namespace/new accounts it is allowed to manage; it does not implicitly grant every existing business customer or add a blanket wildcard to current credentials.
- The application makes gateway calls with a customer-scoped credential or reports external work through explicit metering authority. Persist operation/customer identity and idempotency keys; a failed network response cannot trigger an unbounded second inference or charge.
- Checkout initiation can create a correctly scoped server-quoted order/link; payment confirmation remains a verified merchant event. Machine permissions never allow self-credit, editing historical bills, reading provider/merchant secrets or silently granting staff membership. Apply an explicit customer/account authorization before billing-history or evidence access.
- Expose sufficient narrowly scoped reads and signed business/customer lifecycle notifications for the application to display balance/entitlement, payment and bill state. Use existing outbox/retry/replay primitives where suitable; duplicate and out-of-order notifications cannot reopen cancelled state or duplicate effects.
- Ship one small runnable external AI application example using a separate database/process or clearly separate application store. Start with the current example runtimes rather than a new large framework. It exercises application signup → customer mapping → authorized key/checkout → funded or entitled use → request/metering receipt → customer bill export/verification → suspension/retry/recovery. Its setup may use an owner session once to issue grants; runtime must not store or repeatedly log in with owner credentials.
- Completion: J18 works against the real candidate listener with a clean external consumer; wrong namespace/customer/scope, revoked credentials, altered replay, incomplete payment and restored retry all fail safely or recover exactly as documented. Adding a broad service token alone does not close this task.

### Conditional follow-up units

| ID | Trigger | Required scope if selected |
| --- | --- | --- |
| C01 SaaS lifecycle integration | Promoted into required N21 by the agent's completeness recommendation | Retained ID for discussion history; not a separate optional task |
| C02 Provider account import | A target operator needs #506 beyond manual supported credentials | Reassess provider-supported authentication and maintain credential isolation; old research about provider OAuth is not a current support contract |
| C03 Extra payment/protocol/architecture coverage | A named pilot requires it or the release explicitly advertises it | Add its exact contract, tests, credentials, budget and release artifact; do not substitute it for unfinished primary workflows |

## 6. Execution order and issue discipline

Start with N00 and environment preflight. Then:

1. N01/N02 repair entry points; N03 establishes the operator path. N13 can be developed as an independent issue sequence.
2. N04/N05 complete supply and pricing; N06/N07/N08 complete collection and people; N09 connects readiness to those facts.
3. Implement N11 before final commercial interpretation, then N10/N12 close the operating and evidence journeys; N21 completes the embedded adoption path.
4. Execute N14/N15 within the prepared external scope; N16/N17 establish capacity and recoverability. Final measurements use the integrated N21 implementation as well.
5. Freeze N18, run N19 when participants are ready, and perform only the selected N20 actions.

Logical independence does not authorize additional agents or new chats. Use the current collaboration policy. Serialize heavy Cargo/nextest work on the shared build cache and reserve isolated performance measurements against competing builds/tests.

For each issue: write acceptance → create a fresh task worktree from current main → update OpenAPI if needed → implement the smallest complete behavior → test and inspect → update docs/TODO → push/open PR → merge after the actual required/relevant checks are green, as authorized by the November 8 instruction → synchronize main → remove only the task-owned merged worktree when dependents are safe.

If required review/CI infrastructure blocks a merge, preserve the branch and work on independent tasks. Never fabricate approval or use `--admin`, force push another agent's branch, reset main, or change protection to continue. Historic approval instructions in other planning files do not establish current external authority.

## 7. Acceptance journeys and evidence

| ID | Journey | Pass condition |
| --- | --- | --- |
| J01 | Clean first run | Documented commands reach the intended setup page and current demo completes; no retired credential/top-up assumptions |
| J02 | Empty deployment to open business | Intended owner initializes, configures supply/sale/collection and admits the first customer through product interfaces; no hidden SQL/API preparation after documented setup-token creation |
| J03 | Provider operation | Save/reload, explicit reachability, credential rotation, authorized internal endpoint and staff denial; a pinned in-flight request survives later edits |
| J04 | Sale terms | Default and two distinct customer terms resolve as previewed; concurrent edits/schedules fail clearly; actual bill and sale evidence match |
| J05 | Checkout | PSP/local tier labeled, pending never credits, confirmed payment fulfills once, amount/currency/customer/merchant match, customer state updates |
| J06 | Customer consent | New/existing recipient, wrong email, expiry/revoke/replay, optional mail and explicit registration mode; no staff or funds leaked |
| J07 | Staff authority | Finance/support enroll without UUID knowledge, operate only their capabilities, retain explicit context and lose authority on revocation |
| J08 | First real call | Correct SDK/base URL, whole/SSE behavior, displayed request/charge and immediate evidence; free/fully entitled zero-wallet use succeeds |
| J09 | Refund/recovery | Customer request → finance decision → bounded reservation → PSP result → linked credit note; retry/timeout/partial limits conserve funds |
| J10 | Plan lifecycle | Paid activation, quota exhaustion/overage, renewal failure/success, boundary change/cancel and expiry; no free renewal or double income |
| J11 | Postpaid cycle | Credit cap, finalized statement, partial/full allocation, overdue and reason-specific restoration; closed document remains intact |
| J12 | Financial explanation | Usage/funding/cash/revenue/cost/receivable/refund totals reconcile to source facts; policy and unknown-cost coverage visible |
| J13 | Independent verification | Actual export verifies offline with retained trust inputs; relevant tampering/missing/wrong-scope cases fail; old formats work |
| J14 | Exact artifact | Full CI/candidate identity matches image/frontend/SDK; promotion rejects wrong evidence; deployed version/digest and rollback verified |
| J15 | Capacity/soak | Declared hardware/workload/percentiles, real workers, per-customer conservation and recoverability; D09 verdict stays literal |
| J16 | Upgrade/restore | Separate restored target preserves balances, keys and old proofs; serves a new call; durations and compatibility recorded |
| J17 | Independent adoption | Two actual outsiders complete their own installation and cycle, with assistance and outcomes recorded |
| J18 | Embedded application | An external app provisions/maps an allowed customer, initiates checkout, consumes/meters, reads customer-scoped results and verifies a bill without an owner cookie; isolated scopes and retry/recovery pass |

All changed browser flows cover desktop and approximately 390-pixel mobile layouts, keyboard operation/focus, both existing UI locales, loading/empty/denied/error/retry states and browser refresh. Capture real screenshots using the applicable browser skill during execution. Source review or a successful frontend build cannot claim browser acceptance.

Financial/authority tests must assert absence of forbidden effects, not only HTTP status. Concurrent/replayed orders/refunds/price changes must check balances, reservations, events and immutable evidence. Functional fixtures and actual PSP/provider calls are separate test tiers.

Evidence record fields:

```text
task / child issue / journey
source SHA / candidate manifest / image identity / platform / verifier version
environment: local-isolated | local-stub | provider-sandbox | live-authorized | independent-pilot
actor role / configuration and fixture description (no secrets)
command or browser steps / expected invariants / observed outcomes
passed / failed / skipped / external_pending; skip reason if applicable
sanitized artifact paths / CI run and attempt / screenshots
remaining prerequisite / next action / responsible party
```

Use current `scripts/fmt-check.sh`, strict workspace Clippy, focused nextest suites, cross-crate workspace coverage, doctests and CI-specific process-shared/engine checks as required. Verify actual scratch-DB execution. Frontend scripts currently include `codegen` and `build`, not a general `npm test`. Read current manifests/workflows rather than copying obsolete commands. Verifier changes require WASM plus the packed consumer and actual browser checks.
