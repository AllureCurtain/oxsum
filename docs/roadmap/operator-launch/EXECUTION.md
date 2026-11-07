# Operator launch execution ledger

Updated: 2026-11-08 (Asia/Shanghai). Owner: this task. Umbrella: [#725](https://github.com/AllureCurtain/oxsum/issues/725).

Read this file on every resume, then reconcile actual main, worktrees, issues/PRs, CI and the persistent provider budget before proceeding. The [accepted contract](CONTRACT.md) preserves every N/R/J requirement; the [baseline evidence](evidence/baseline.md) distinguishes observed results from pending candidate acceptance.

## Current state

- Starting and last synchronized main: `0314cbd3d5439adbc904a327a8e16704b344a0af`. No previous task PR/worktree remains. #697 and #724 are complete.
- N00 has adopted the full contract and opened all work-unit issues; PR #749 passed required CI and local fmt/strict Clippy/coverage checks; Entire Gates awaits real reviewer approval (trail 154). No software unit is complete by adoption.
- No candidate for this stage has been frozen. Historical candidate `813c788` and baseline CI are references only.
- Fifteen submitted PRs (#749, #750, #751, #753, #755, #758, #761, #762, #764, #766, #767, #771, #772, #773, #775) pass required code CI. Entire Gates requires genuine reviewer approval. Continue independent software work from current main; no review bypass or candidate completion is implied.

## Task graph

Statuses distinguish planned, in_progress, implemented, validated, in_review, integrated and external_pending. A unit can retain external_pending acceptance after software integration. An absent PR/commit means no implementation has integrated.

| Unit / owner issue | Depends on | Status | Worktree / branch | PR / commit | Evidence / blocker / next action |
| --- | --- | --- | --- | --- | --- |
| N00 [#726](https://github.com/AllureCurtain/oxsum/issues/726) — Rebaseline and adopt the execution contract | baseline | in_review | `.worktrees/operator-launch-contract` / `docs/operator-launch-contract` | [#749](https://github.com/AllureCurtain/oxsum/pull/749) / `9ff2d87` | [Baseline](evidence/baseline.md); Entire Gates approvals pending; continue independent implementation |
| N01 [#727](https://github.com/AllureCurtain/oxsum/issues/727) — Align current product claims and documentation | N00 | in_review | `.worktrees/current-product-docs` / `docs/current-product-claims` | [#767](https://github.com/AllureCurtain/oxsum/pull/767) / `1d7d545` | 15 documents aligned; source/link/fmt/diff/strict Clippy passed; CI 42413369646 passed; Entire trail164 awaits genuine reviewer approval |
| N02 [#728](https://github.com/AllureCurtain/oxsum/issues/728) — Repair the runnable demo and first-run example | N00 | in_review | `.worktrees/current-demo` / `fix/current-demo` | [#751](https://github.com/AllureCurtain/oxsum/pull/751) / `66f7dee` | Exact settlement/offline rejection, fresh/existing walkthroughs, isolation tests passed; CI passed; Entire trail 156 awaits reviewer approval. Isolation child #769 submitted as PR #772 / `fb4f9d5`; 8 failure-path checks, repeated 52-assertion artifact walkthroughs and default build/pack path pass; CI passed, Entire trail166 review pending; requires #751 integration |
| N03 [#729](https://github.com/AllureCurtain/oxsum/issues/729) — Owner setup, business profile and operating settings | N00 | in_progress | `.worktrees/owner-browser-setup` / `feat/owner-browser-setup` | [#750](https://github.com/AllureCurtain/oxsum/pull/750) / `fc9c23a` (child #748) | Build/fmt/strict Clippy, 3 focused backend tests and browser new/existing/wrong-email/replay/recovery checks passed; required CI passed; Entire trail 155 awaits actual reviewer approval. Profile child #756 submitted as PR #758 / `917f918`; 1,810 workspace tests/build/fmt/strict Clippy passed; CI 42401566984 passed; Entire trail159 awaits approval; browser acceptance pending; currency child #765 submitted as PR #771 / `96ec4bd`; final18 focused passes, strict Clippy/build pass. Full final run: 1,813 passed/1 job-clock fixture failed/6 opt-ins skipped; #770 isolated fix required, do not call this full green |
| N04 [#730](https://github.com/AllureCurtain/oxsum/issues/730) — Business provider/channel administration | N03 | in_progress | `.worktrees/business-providers` / `feat/business-providers` | [#755](https://github.com/AllureCurtain/oxsum/pull/755) / `c8c7327` (child #754) | Owner editor implemented; build/fmt/strict Clippy, 11 backend checks and actual listener replay/rotation passed. CI 42398109401 passed, Entire trail158 awaits approval; browser space disappeared: desktop/mobile/retry acceptance pending; model/routing child follows |
| N05 [#731](https://github.com/AllureCurtain/oxsum/issues/731) — Sale-book creation, publication and assignment | N04 | in_review | `.worktrees/sale-book-editor` / `feat/sale-book-editor` | [#775](https://github.com/AllureCurtain/oxsum/pull/775) / `d5c29b3` | Full editor submitted; exact-money/UTC tests, 11 PostgreSQL tests, frontend/fmt/strict Clippy pass. Actual two-customer preview23/46 equals settlement; midstream revision69 preserves admitted23; offline sale/price/head/tamper checks pass. CI 42421954534 passed after moving exact-input checks into frontend job; Entire trail168 review and actual browser acceptance pending |
| N06 [#732](https://github.com/AllureCurtain/oxsum/issues/732) — Merchant configuration and payment readiness | N03 | in_progress | `.worktrees/hyperswitch-sandbox` / `build/hyperswitch-sandbox` | [#762](https://github.com/AllureCurtain/oxsum/pull/762) / `dbb4ff4` (child #757) | CI 42405627919 passed; Entire trail161 approval pending. Pinned stack healthy; identities/mail survive forced recreation; actual USD 2 / USD 0.50 local-switch smoke passes with #759. HTTPS tunnel TLS fails; automatic callback/browser/PSP remain pending. Merchant UI/selection remains separate |
| N07 [#733](https://github.com/AllureCurtain/oxsum/issues/733) — Customer invitation and registration controls | N03 | planned | — | — | Complete invitation management and captured mail |
| N08 [#734](https://github.com/AllureCurtain/oxsum/issues/734) — Staff invitations and role lifecycle | N03 | planned | — | — | Implement consent-bound staff invitations |
| N09 [#735](https://github.com/AllureCurtain/oxsum/issues/735) — Journey-specific readiness and first bill | N03–N08,N10 | planned | — | — | Derive readiness from authoritative role/entitlement facts |
| N10 [#736](https://github.com/AllureCurtain/oxsum/issues/736) — Complete commercial journeys and targeted repairs | N05–N09,N11 | in_progress | `.worktrees/refund-correlation` / `fix/refund-correlation`; `.worktrees/hyperswitch-recovery` / `fix/hyperswitch-recovery` | [#761](https://github.com/AllureCurtain/oxsum/pull/761) / `477e03f` (#759); child #760 | #759 correlation fix: 25 focused and 1,811 workspace tests passed (6 opt-ins skipped), fmt/Clippy and local-switch smoke pass; CI 42405405663 passed; Entire trail160 approval pending. #760 submitted as PR #764 / `1f63ce0`: final 1,818 workspace tests passed/6 opt-ins skipped, strict Clippy/build/fmt and local switch fault smoke passed. #763 submitted as PR #766 / `fee44e4`: 1,813 PostgreSQL workspace passes/6 opt-ins skipped, strict Clippy/build/fmt and pinned switch smoke passed. Both code CI runs passed; Entire trails162/163 await genuine approval. Commercial authority umbrella #768: bundle child #774 in `.worktrees/bundle-offers` / `fix/bundle-offers`, immutable offers and authoritative grants implemented, final UI/boundary/full validation underway; plan authority child #776 is in `.worktrees/plan-payment-authority` / `fix/plan-payment-authority`, stacked on #771. Quote/snapshot/paid activation/renewal isolation and portal flow implemented, validation pending. Explicit statement repayment child #777 retains partial/full debt repayment and recovery; full journeys remain |
| N11 [#737](https://github.com/AllureCurtain/oxsum/issues/737) — Implement the agreed revenue policy and prove reconciliation | N00 | planned | — | — | Design versioned daily recognition and conservation fixtures |
| N12 [#738](https://github.com/AllureCurtain/oxsum/issues/738) — Independent evidence delivery and useful verification | N09–N11 | planned | — | — | Make actual exports independently usable offline |
| N13 [#739](https://github.com/AllureCurtain/oxsum/issues/739) — Promote the validated artifact and deploy by identity | N00 | in_progress | `.worktrees/candidate-artifact` / `build/candidate-artifact` | [#753](https://github.com/AllureCurtain/oxsum/pull/753) / `be12af2` (child #752) | 22 gate tests, real Docker archive-format fixture, exact packed SDK consumer, actionlint/fmt/strict Clippy passed; CI 42396428383 green; Entire trail 157 awaits review; promotion/deploy child follows |
| N14 [#740](https://github.com/AllureCurtain/oxsum/issues/740) — Real-provider certification for the selected matrix | N04,N05,N12 | planned | — | — | Bound the harness; verify price/DNS before any attempt |
| N15 [#741](https://github.com/AllureCurtain/oxsum/issues/741) — Primary PSP sandbox and bounded live certification | N06,N10,N12 | planned | — | — | Complete fixtures now; leave genuine PSP rows pending |
| N16 [#742](https://github.com/AllureCurtain/oxsum/issues/742) — Capacity, fairness and long-duration operation | N04–N12,N21 | in_progress | `.worktrees/job-fixture-clock` / `test/job-fixture-clock` | [#773](https://github.com/AllureCurtain/oxsum/pull/773) / `ca1d22f` (#770) | Host/database clock mismatch reproduced in 199/200 probes; 38 job/fault tests plus 5 repeated checks passed. Fmt/strict Clippy/CI passed; Entire trail167 review pending. Capacity and soak remain |
| N17 [#743](https://github.com/AllureCurtain/oxsum/issues/743) — Operator diagnostics, upgrade and restore | N13; final after N16 | planned | — | — | Prepare diagnostics and isolated upgrade/restore rehearsal |
| N18 [#744](https://github.com/AllureCurtain/oxsum/issues/744) — Final integrated candidate acceptance | N01–N17,N21 | planned | — | — | Freeze only after dependencies; run exact artifact checks |
| N19 [#745](https://github.com/AllureCurtain/oxsum/issues/745) — Two independent operator pilots | N18 + participants | planned | — | — | Prepare both pilot kits; no external contact |
| N20 [#746](https://github.com/AllureCurtain/oxsum/issues/746) — Publication and deployment, if selected | N18 + authority/gates | planned | — | — | Prepare/rehearse tooling; public actions unperformed |
| N21 [#747](https://github.com/AllureCurtain/oxsum/issues/747) — Complete embedded application integration | N03,N05–N07,N10–N12 | planned | — | — | Split namespace/grants, lifecycle APIs and external example |

## Requirement ownership

| Requirement | Unit issue ownership | Journeys | Verdict |
| --- | --- | --- | --- |
| R01 | N01/#727, N02/#728, N03/#729 | J01 | pending |
| R02 | N03/#729, N08/#734, N09/#735 | J02,J07 | pending |
| R03 | N04/#730 | J03 | pending |
| R04 | N05/#731 | J04 | pending |
| R05 | N06/#732, N10/#736, N15/#741 | J05,J09 | pending |
| R06 | N07/#733 | J06 | pending |
| R07 | N09/#735 | J02,J08 | pending |
| R08 | N10/#736, N11/#737 | J09–J12 | pending |
| R09 | N12/#738, N18/#744 | J13 | pending |
| R10 | N13/#739, N18/#744, N20/#746 | J14 | pending |
| R11 | N14/#740, N15/#741 | J05,J08,J09 | pending |
| R12 | N16/#742, N17/#743 | J15,J16 | pending |
| R13 | N19/#745 | J17 | pending |
| R14 | N21/#747 | J18 | pending |

## Journey evidence

| Journey | Accountable unit issues | Current tier / verdict | Evidence / next action |
| --- | --- | --- | --- |
| J01 | N01/#727, N02/#728 | pending | Execute the complete [contract journey](CONTRACT.md#7-acceptance-journeys-and-evidence); keep fixture/provider/external tiers separate. |
| J02 | N03/#729, N09/#735 | pending | Execute the complete [contract journey](CONTRACT.md#7-acceptance-journeys-and-evidence); keep fixture/provider/external tiers separate. |
| J03 | N04/#730 | pending | Execute the complete [contract journey](CONTRACT.md#7-acceptance-journeys-and-evidence); keep fixture/provider/external tiers separate. |
| J04 | N05/#731 | pending | Execute the complete [contract journey](CONTRACT.md#7-acceptance-journeys-and-evidence); keep fixture/provider/external tiers separate. |
| J05 | N06/#732, N15/#741 | pending | Execute the complete [contract journey](CONTRACT.md#7-acceptance-journeys-and-evidence); keep fixture/provider/external tiers separate. |
| J06 | N07/#733 | pending | Execute the complete [contract journey](CONTRACT.md#7-acceptance-journeys-and-evidence); keep fixture/provider/external tiers separate. |
| J07 | N08/#734 | pending | Execute the complete [contract journey](CONTRACT.md#7-acceptance-journeys-and-evidence); keep fixture/provider/external tiers separate. |
| J08 | N09/#735, N14/#740 | pending | Execute the complete [contract journey](CONTRACT.md#7-acceptance-journeys-and-evidence); keep fixture/provider/external tiers separate. |
| J09 | N10/#736, N15/#741 | pending | Execute the complete [contract journey](CONTRACT.md#7-acceptance-journeys-and-evidence); keep fixture/provider/external tiers separate. |
| J10 | N10/#736, N11/#737 | pending | Execute the complete [contract journey](CONTRACT.md#7-acceptance-journeys-and-evidence); keep fixture/provider/external tiers separate. |
| J11 | N10/#736, N11/#737 | pending | Execute the complete [contract journey](CONTRACT.md#7-acceptance-journeys-and-evidence); keep fixture/provider/external tiers separate. |
| J12 | N11/#737 | pending | Execute the complete [contract journey](CONTRACT.md#7-acceptance-journeys-and-evidence); keep fixture/provider/external tiers separate. |
| J13 | N12/#738 | pending | Execute the complete [contract journey](CONTRACT.md#7-acceptance-journeys-and-evidence); keep fixture/provider/external tiers separate. |
| J14 | N13/#739, N18/#744, N20/#746 | pending | Execute the complete [contract journey](CONTRACT.md#7-acceptance-journeys-and-evidence); keep fixture/provider/external tiers separate. |
| J15 | N16/#742 | pending | Execute the complete [contract journey](CONTRACT.md#7-acceptance-journeys-and-evidence); keep fixture/provider/external tiers separate. |
| J16 | N17/#743 | pending | Execute the complete [contract journey](CONTRACT.md#7-acceptance-journeys-and-evidence); keep fixture/provider/external tiers separate. |
| J17 | N19/#745 | pending | Execute the complete [contract journey](CONTRACT.md#7-acceptance-journeys-and-evidence); keep fixture/provider/external tiers separate. |
| J18 | N21/#747 | pending | Execute the complete [contract journey](CONTRACT.md#7-acceptance-journeys-and-evidence); keep fixture/provider/external tiers separate. |

## Environment and action boundaries

| Item | Current state | Next action / affected gate |
| --- | --- | --- |
| E0 local development | ready: toolchain, PostgreSQL 17, isolated worktrees | Serialize heavy Cargo work; preserve other projects/resources |
| E1 browser/mail fixtures | task-owned databases retained; provider listener/HEAD fixture stopped; local Mailpit capture ready | Browser space4 disappeared; no unrelated user space claimed. Browser acceptance awaits recovery; continue non-browser work |
| E2 DeepSeek | credential privately available; no calls made | N14 verify current pricing, bound/reserve attempts, resolve DNS without weakening public-address policy |
| E3 Hyperswitch | pinned task stack running; fresh keys/merchant/dummy connector persist | N06 public HTTPS blocked by tunnel TLS; complete merchant UI/selection and #760 retry safety |
| Genuine PSP sandbox | external_pending: usable test credential absent | N15 needs Stripe test-account API credential in the fresh Hyperswitch test connector; fixture tests proceed |
| E4 reference capacity | external_pending for qualifying host; local diagnostics pending | Current aarch64/4-GiB Docker is not native amd64/4-vCPU/8-GiB. Do not relabel emulation or lower D09 thresholds |
| E5 live payment | unperformed; excluded from this run | Named payer/provider/amount authority needed for eventual certification |
| E6 independent pilots | participants absent; kit preparation pending | N19 prepare portal and embedded walkthroughs; actual two outsiders required for J17 |
| E7 publication/remote upgrade | unperformed; excluded from this run | N13/N20 tooling and isolated rehearsal proceed; no dispatch of publishing/deploy workflows |
| Mail | local capture only | No external delivery/recipient requirement for local acceptance |

## Shared real-provider expense record

Scope: the entire November 8 operator-launch execution, including all PRs, resumed contexts and processes. Ceiling **CNY 0.10 total**, default two inference calls, maximum **five upstream attempts**, at most **32 output tokens** per attempt, thinking explicitly disabled. Retries, failures and ambiguous timeouts consume attempts. No other paid service or automatic recharge.

Durable machine record (outside worktrees, ignored): `local-notes/operator-launch/provider-budget.json`. Read it before any live attempt; reserve the conservative bound and fsync before network transmission. If it is missing, inconsistent or exhausted, fail closed until reconstructed from retained evidence. Do not reset it when creating a worktree. A new worktree does not create a new budget.

| Attempts used / maximum | Reserved CNY | Reconciled CNY | Price verification | Result |
| --- | --- | --- | --- | --- |
| 0 / 5 | 0 | 0 | pending before first call | No live inference or credential authentication attempted |

Use integer CNY millionths in the machine record (`100000` ceiling). Before sending, record provider/model/price source/date, serialized input bound, output cap, reserved cost and attempt identity. Afterward retain sanitized usage/result/cost evidence. Account for ambiguous outcomes conservatively; oxsum wallet freezes do not enforce the upstream cash budget.

## Integration and acceptance events

| Date | Event | Source / evidence | Next action |
| --- | --- | --- | --- |
| 2026-11-08 | Baseline reconciled; #725 and N00–N21 issues created | `0314cbd`; [baseline](evidence/baseline.md); zero attempts/spend | Integrate N00; continue N01/N02/N03/N13 |

| 2026-11-08 | N03.a initial owner browser flow submitted | PR #750 / `fc9c23a`; [development evidence](https://github.com/AllureCurtain/oxsum/blob/fc9c23a/docs/roadmap/operator-launch/evidence/owner-browser-setup.md) | CI passed; reviewer pending; remaining N03 profile/settings still required |
| 2026-11-08 | N02 demo submitted | PR #751 / `66f7dee`; exact 34-minor settlement, offline verification and tamper rejection; zero provider spend | Review pending; no final candidate claim |
| 2026-11-08 | N13.a candidate byte/provenance gates submitted | PR #753 / `be12af2`; [development evidence](https://github.com/AllureCurtain/oxsum/blob/be12af2/docs/roadmap/operator-launch/evidence/candidate-format.md); CI 42396428383 green | Real review pending; continue promotion/deployment implementation |
| 2026-11-08 | N04.a connection editor started | Child #754; fresh-main worktree `feat/business-providers` | Implement, browser-check and submit owner authority workflow |

| 2026-11-08 | N04.a owner provider editor submitted | PR #755 / `c8c7327`; [development evidence](https://github.com/AllureCurtain/oxsum/blob/c8c7327/docs/roadmap/operator-launch/evidence/business-providers.md); 11 tests and listener checks passed | CI/review and browser acceptance pending; no live inference/spend |

| 2026-11-08 | N03.b persistent business profile submitted | PR #758 / `917f918`; 1,810 workspace tests passed after migration inventory fix; [development evidence](https://github.com/AllureCurtain/oxsum/blob/917f918/docs/roadmap/operator-launch/evidence/business-profile.md) | CI passed; real review and browser acceptance pending; no spend |

| 2026-11-08 | N06 pinned local switch persistence and signed-event smoke passed | Child #757; source `cf913c5`; #759 binary `7a328782…c193a`; USD 2 paid / USD 0.50 simulated refund / 1,500,000 credit-minor ending balance | Automatic callback/browser/real PSP pending; all temporary failed tunnels stopped |
| 2026-11-08 | N10 refund callback correlation repair and retry audit | #759: immutable intake + trusted exact-version correlation; 25 focused tests passed. #760: discovered body IDs required by pinned switch, and ambiguous retries must retain reservations | #759 submitted as PR761 with 1,811 workspace tests passed; #757 submitted as PR762; implement #760 without transferring old attempts to new identities |

| 2026-11-08 | N10 body identity and lost-response recovery exercised | #760 uncommitted source; 53 focused tests passed plus corrected fixture rerun; actual pinned switch smoke `10cff05172cf`, binary `e2a779bb…dfccef`: one create per operation despite two dropped responses; duplicate refund `HE_01`; exact 1,500,000 ending balance | Workspace test active; review pending. Legacy admin reversal adapter isolated as #763; no provider spend |

| 2026-11-08 | N10 recovery hardening and N01 claim audit | #760 first workspace run: 1,815 passed/6 skipped; edge checks cover real timeout, history, cross-order ID and fair recovery paging. One repeated-setup fixture fixed and passed. Final fmt/Clippy/workspace/build/switch sequence active. N01 source audit corrected setup, proof scope, historical versus current certification across 14 documents | Submit after final validation; 0 provider attempts/spend; genuine approval still missing on all eight submitted PRs |

| 2026-11-08 | N10 order recovery submitted and legacy reversal advanced | PR #764 / `1f63ce0`; 1,818 workspace passes, 6 opt-in skips; pinned switch smoke `e35ff27c2c6c`, binary `7b60c71e…d9ffc30`, one create per operation under response loss. #763 has 14 actual PostgreSQL-backed test passes; new edge/upgrade tests active | CI/review pending; no paid model attempts or spend |

| 2026-11-08 | Legacy reversal recovery submitted; collection authority defect isolated | PR #766 / `fee44e4`: 1,813 workspace passes, exact pinned-switch lost-response smoke. #765 rejects customer-controlled collection conversion; core/UI implemented, 29 focused core passes, HTTP capability fixture issue fixed and rerun pending | Code CI/review and remaining final checks; no real-model attempts/spend |

| 2026-11-08 | N01 claims submitted; collection and example isolation verified | PR #767 / `1d7d545`, CI 42413369646 passed, Entire trail164 review pending. #765: 1,813 workspace passes/6 opt-ins skipped, fmt/strict Clippy/frontend/server build passed; additional boundary tests active. #769: eight isolation unit checks passed, actual frozen-artifact walkthrough running with PR #751 source | Submit remaining fixes after verification. #768 accepted for commercial authority. No actual model requests/spend; no merges or candidate freeze |

| 2026-11-08 | Collection submitted; sales and isolated examples exercised | PR #771 / `96ec4bd`. Final full run has one measured job-clock fixture failure; #770 is independent and must be integrated/rechecked. Sales local binary `d1ee30d7…69fe6` preserved23 under midstream default69 and billed secondcustomer46; signed evidence verified offline. #769 defaultbuild/pack plus52essentialassertions pass | No merge/candidate claim; strict Clippy/review/browser gates remain; 0 paid attempts/spend |

## Resume procedure

1. Read this ledger and the accepted contract; inspect ignored `local-notes/operator-launch/state.json` and `provider-budget.json` without printing credentials.
2. Fetch/reconcile main and check open PRs/CI/worktrees. Preserve unrelated edits and resource ownership. A merged PR is not pending work.
3. Select the earliest dependency-ready unfinished issue; use its fresh worktree, contract-first implementation, meaningful checks and normal protected integration.
4. Update task/R/J evidence after merges and acceptance, and both expense records after each reservation/outcome. Keep all unfinished software visible.
5. Freeze a candidate only after software dependencies. Run executed full CI including engine/process-shared suites and actual artifact browser/embedded/offline/deploy/restore checks. Never transfer an older pass to changed code.

| 2026-11-08 | Reconciled all fifteen submitted PRs | Main remains `0314cbd`; required code CI passed. #771/#772/#773/#775 Entire trails165–168 explicitly report no reviewer approval. Budget remains 0/5 attempts and CNY0 | Preserve real review gate; continue bundle child #774 and remaining independent work |

| 2026-11-08 | Fresh reconciliation and plan authority split | Remote main remains `0314cbd`; #775 `d5c29b3` required code CI42421954534 green, genuine review pending. #774 strict Clippy passed; full workspace test enumeration continues after successful compilation. #776 implementation proceeds on #771; #777 created before its software work to preserve explicit statement repayment | No merges/candidate freeze, browser acceptance or provider spend; 0/5 live attempts |
