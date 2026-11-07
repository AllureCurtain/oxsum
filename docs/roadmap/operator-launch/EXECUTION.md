# Operator launch execution ledger

Updated: 2026-11-08 (Asia/Shanghai). Owner: this task. Umbrella: [#725](https://github.com/AllureCurtain/oxsum/issues/725).

Read this file on every resume, then reconcile actual main, worktrees, issues/PRs, CI and the persistent provider budget before proceeding. The [accepted contract](CONTRACT.md) preserves every N/R/J requirement; the [baseline evidence](evidence/baseline.md) distinguishes observed results from pending candidate acceptance.

## Current state

- Starting and last synchronized main: `0314cbd3d5439adbc904a327a8e16704b344a0af`. No previous task PR/worktree remains. #697 and #724 are complete.
- N00 has adopted the full contract and opened all work-unit issues; PR #749 passed required CI and local fmt/strict Clippy/coverage checks; Entire Gates awaits real reviewer approval (trail 154). No software unit is complete by adoption.
- No candidate for this stage has been frozen. Historical candidate `813c788` and baseline CI are references only.
- Four submitted PRs (#749, #750, #751, #753) pass required code CI; Entire Gates requires genuine reviewer approval. Continue independent software work from current main; no review bypass or candidate completion is implied.

## Task graph

Statuses distinguish planned, in_progress, implemented, validated, in_review, integrated and external_pending. A unit can retain external_pending acceptance after software integration. An absent PR/commit means no implementation has integrated.

| Unit / owner issue | Depends on | Status | Worktree / branch | PR / commit | Evidence / blocker / next action |
| --- | --- | --- | --- | --- | --- |
| N00 [#726](https://github.com/AllureCurtain/oxsum/issues/726) — Rebaseline and adopt the execution contract | baseline | in_review | `.worktrees/operator-launch-contract` / `docs/operator-launch-contract` | [#749](https://github.com/AllureCurtain/oxsum/pull/749) / `9016b2e` | [Baseline](evidence/baseline.md); Entire Gates approvals pending; continue independent implementation |
| N01 [#727](https://github.com/AllureCurtain/oxsum/issues/727) — Align current product claims and documentation | N00 | planned | — | — | Align current claims without rewriting historical evidence |
| N02 [#728](https://github.com/AllureCurtain/oxsum/issues/728) — Repair the runnable demo and first-run example | N00 | in_review | `.worktrees/current-demo` / `fix/current-demo` | [#751](https://github.com/AllureCurtain/oxsum/pull/751) / `66f7dee` | Exact settlement/offline rejection, fresh/existing walkthroughs, isolation tests passed; CI passed; Entire trail 156 awaits reviewer approval |
| N03 [#729](https://github.com/AllureCurtain/oxsum/issues/729) — Owner setup, business profile and operating settings | N00 | in_progress | `.worktrees/owner-browser-setup` / `feat/owner-browser-setup` | [#750](https://github.com/AllureCurtain/oxsum/pull/750) / `fc9c23a` (child #748) | Build/fmt/strict Clippy, 3 focused backend tests and browser new/existing/wrong-email/replay/recovery checks passed; required CI passed; Entire trail 155 awaits actual reviewer approval. Profile child #756 implemented in `.worktrees/business-profile`; focused tests/build/strict Clippy passed, full workspace running; currency child pending |
| N04 [#730](https://github.com/AllureCurtain/oxsum/issues/730) — Business provider/channel administration | N03 | in_progress | `.worktrees/business-providers` / `feat/business-providers` | [#755](https://github.com/AllureCurtain/oxsum/pull/755) / `c8c7327` (child #754) | Owner editor implemented; build/fmt/strict Clippy, 11 backend checks and actual listener replay/rotation passed. CI 42398109401 passed, Entire trail158 awaits approval; browser space disappeared: desktop/mobile/retry acceptance pending; model/routing child follows |
| N05 [#731](https://github.com/AllureCurtain/oxsum/issues/731) — Sale-book creation, publication and assignment | N04 | planned | — | — | Complete exact revision editor and activation/assignment |
| N06 [#732](https://github.com/AllureCurtain/oxsum/issues/732) — Merchant configuration and payment readiness | N03 | planned | — | — | Complete merchant UI and pinned persistent switch stack |
| N07 [#733](https://github.com/AllureCurtain/oxsum/issues/733) — Customer invitation and registration controls | N03 | planned | — | — | Complete invitation management and captured mail |
| N08 [#734](https://github.com/AllureCurtain/oxsum/issues/734) — Staff invitations and role lifecycle | N03 | planned | — | — | Implement consent-bound staff invitations |
| N09 [#735](https://github.com/AllureCurtain/oxsum/issues/735) — Journey-specific readiness and first bill | N03–N08,N10 | planned | — | — | Derive readiness from authoritative role/entitlement facts |
| N10 [#736](https://github.com/AllureCurtain/oxsum/issues/736) — Complete commercial journeys and targeted repairs | N05–N09,N11 | planned | — | — | Run all commercial cycles and file focused repairs |
| N11 [#737](https://github.com/AllureCurtain/oxsum/issues/737) — Implement the agreed revenue policy and prove reconciliation | N00 | planned | — | — | Design versioned daily recognition and conservation fixtures |
| N12 [#738](https://github.com/AllureCurtain/oxsum/issues/738) — Independent evidence delivery and useful verification | N09–N11 | planned | — | — | Make actual exports independently usable offline |
| N13 [#739](https://github.com/AllureCurtain/oxsum/issues/739) — Promote the validated artifact and deploy by identity | N00 | in_progress | `.worktrees/candidate-artifact` / `build/candidate-artifact` | [#753](https://github.com/AllureCurtain/oxsum/pull/753) / `be12af2` (child #752) | 22 gate tests, real Docker archive-format fixture, exact packed SDK consumer, actionlint/fmt/strict Clippy passed; CI 42396428383 green; Entire trail 157 awaits review; promotion/deploy child follows |
| N14 [#740](https://github.com/AllureCurtain/oxsum/issues/740) — Real-provider certification for the selected matrix | N04,N05,N12 | planned | — | — | Bound the harness; verify price/DNS before any attempt |
| N15 [#741](https://github.com/AllureCurtain/oxsum/issues/741) — Primary PSP sandbox and bounded live certification | N06,N10,N12 | planned | — | — | Complete fixtures now; leave genuine PSP rows pending |
| N16 [#742](https://github.com/AllureCurtain/oxsum/issues/742) — Capacity, fairness and long-duration operation | N04–N12,N21 | planned | — | — | Prepare controlled workload and environment diagnostics |
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
| E1 browser/mail fixtures | task-owned databases retained; provider listener and HEAD fixture stopped; mail pending | Browser space4 disappeared; no unrelated user space claimed. Browser acceptance awaits recovery; continue non-browser work |
| E2 DeepSeek | credential privately available; no calls made | N14 verify current pricing, bound/reserve attempts, resolve DNS without weakening public-address policy |
| E3 Hyperswitch | disposable rebuild authorized; old stack absent | N06 pin compatible services, persistent volumes, fresh keys, consistent HTTPS SDK/API/callback origins |
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

## Resume procedure

1. Read this ledger and the accepted contract; inspect ignored `local-notes/operator-launch/state.json` and `provider-budget.json` without printing credentials.
2. Fetch/reconcile main and check open PRs/CI/worktrees. Preserve unrelated edits and resource ownership. A merged PR is not pending work.
3. Select the earliest dependency-ready unfinished issue; use its fresh worktree, contract-first implementation, meaningful checks and normal protected integration.
4. Update task/R/J evidence after merges and acceptance, and both expense records after each reservation/outcome. Keep all unfinished software visible.
5. Freeze a candidate only after software dependencies. Run executed full CI including engine/process-shared suites and actual artifact browser/embedded/offline/deploy/restore checks. Never transfer an older pass to changed code.
