# Operator-launch integration reconciliation (#794)

Date: 2026-11-08 (Asia/Shanghai). Inspected source: `c79a588455b31e9cc7ceafd9341806a83f1dfc62`. This checkpoint reconciles the initial Codex work and the subsequent Claude Code integration batch. It is a repository/GitHub/source review, not a new runtime acceptance run.

## Observed integration

Local and remote main matched the inspected SHA. All 26 PRs below were merged; the open-PR query returned no pending PR from this batch. Five N-unit owner issues were closed (#726, #727, #728, #733, #734); 17 remained open. These are issue states, not a completion percentage. In particular, customer/staff invitation browser acceptance remains unperformed.

| PR | Merge commit | Merged at (Asia/Shanghai) | Change |
| --- | --- | --- | --- |
| [#749](https://github.com/AllureCurtain/oxsum/pull/749) | `8df5cdc53f051ec4cf734909a61e3a367014d332` | 2026-11-08 09:49:36 | docs: adopt operator launch contract and execution ledger |
| [#773](https://github.com/AllureCurtain/oxsum/pull/773) | `81c1500174017bffa387a2b4914e153db55fdfbd` | 2026-11-08 09:53:51 | test: use database time in the job retry fault fixture |
| [#783](https://github.com/AllureCurtain/oxsum/pull/783) | `2e11e9bc692e23e6edcc28d4e05c750873b1ea70` | 2026-11-08 10:01:53 | docs: record the versioned plan-fee recognition policy (N11) |
| [#786](https://github.com/AllureCurtain/oxsum/pull/786) | `f9f637dde3aaa340457311758b1c6668c76a0638` | 2026-11-08 10:02:38 | feat(core,server): scoped application customer provisioning (N21.a) |
| [#750](https://github.com/AllureCurtain/oxsum/pull/750) | `4f45c8953d5b8cec96553b98df1965578927d4ba` | 2026-11-08 10:03:12 | feat(web): add initial owner setup and recovery |
| [#787](https://github.com/AllureCurtain/oxsum/pull/787) | `7e68b1aa1b6da0849e6b15da78cb22702e642594` | 2026-11-08 10:03:29 | docs: record N21.a scoped provisioning PR #786 in the execution ledger |
| [#755](https://github.com/AllureCurtain/oxsum/pull/755) | `a78ae0438f436448b6b2f1a58f8598d62f0842f0` | 2026-11-08 10:05:03 | feat(web): manage owner provider connections and credentials |
| [#751](https://github.com/AllureCurtain/oxsum/pull/751) | `4618446bad058d1edfefc5f0945d9c17b388a8f4` | 2026-11-08 10:08:27 | fix(demo): use isolated current-authority billing flow |
| [#762](https://github.com/AllureCurtain/oxsum/pull/762) | `db2dca89ca3c542beeb28e9023319624dfbc345a` | 2026-11-08 10:11:24 | build: pin and isolate the disposable Hyperswitch sandbox |
| [#767](https://github.com/AllureCurtain/oxsum/pull/767) | `03f21e9ff128c1de19898579d337f14f73ed1175` | 2026-11-08 10:11:52 | docs: align product authority and verification claims with executed evidence |
| [#784](https://github.com/AllureCurtain/oxsum/pull/784) | `65e8b0773439df90ff8243430648e86c779a3b9e` | 2026-11-08 10:12:57 | fix(core): synchronize plan statements with journal settled and void state (#779) |
| [#790](https://github.com/AllureCurtain/oxsum/pull/790) | `fc3765e56b9d8e1754913a9f25c4afa369a2eaf6` | 2026-11-08 10:17:04 | docs: record N21.b scoped billing read PR #789 in the execution ledger |
| [#753](https://github.com/AllureCurtain/oxsum/pull/753) | `1cc5e0a4bb3ec930d691e52ca06775baf6efbda8` | 2026-11-08 10:20:48 | build: bind candidates to executed gates and exact artifacts |
| [#792](https://github.com/AllureCurtain/oxsum/pull/792) | `4f2f8f6df41570d1d8c72fbf86f3690d0381c64f` | 2026-11-08 10:22:39 | docs: correct N21.b ledger entry to PR #791 / 6424800 |
| [#764](https://github.com/AllureCurtain/oxsum/pull/764) | `c7926feef3fc5f9d72f3a92f4fe2de743e076971` | 2026-11-08 10:26:49 | fix: recover Hyperswitch payments and refunds without duplicate creation |
| [#772](https://github.com/AllureCurtain/oxsum/pull/772) | `dd945d77cfb7fa915cac798ff7b812e5f1ebdea1` | 2026-11-08 10:32:41 | fix: isolate example provisioning and exact artifact verification |
| [#781](https://github.com/AllureCurtain/oxsum/pull/781) | `392fe74bdaf227d5b9f6cf885e414aec907963c0` | 2026-11-08 10:34:00 | feat(core): deliver and scope business customer invitations |
| [#758](https://github.com/AllureCurtain/oxsum/pull/758) | `8169ef04382af0dd802608dff0267e966ec6b722` | 2026-11-08 10:40:12 | feat: persist owner-managed business identity and help settings |
| [#775](https://github.com/AllureCurtain/oxsum/pull/775) | `89c59e95bb4def8bfcea14788cb9b4f9f33e01fc` | 2026-11-08 10:48:23 | feat: complete sale-book editing publication and activation in the portal |
| [#791](https://github.com/AllureCurtain/oxsum/pull/791) | `d2b12ab2715e6622c31ae0f50f0ff67eecfed55a` | 2026-11-08 11:15:07 | feat(core,server): scoped application customer billing reads (N21.b) |
| [#761](https://github.com/AllureCurtain/oxsum/pull/761) | `c9297d79765918f8945379f83f97b05c0ee1ec30` | 2026-11-08 11:19:29 | fix: correlate signed refund callbacks within pinned merchant versions |
| [#782](https://github.com/AllureCurtain/oxsum/pull/782) | `ab39dded48cdc68087c4f49affe264f648e3d463` | 2026-11-08 11:20:26 | feat(core,server): business staff invitations bound to role and email |
| [#771](https://github.com/AllureCurtain/oxsum/pull/771) | `8a9df3cfc855550fc7b60acf5b08d0e54fc8bc8f` | 2026-11-08 11:31:27 | fix: make collection conversion authoritative business policy |
| [#766](https://github.com/AllureCurtain/oxsum/pull/766) | `90a25ea176d54be7aeef1ff677999d3d85b6713a` | 2026-11-08 11:37:16 | fix: preserve legacy deposit refund identity and unknown outcomes |
| [#778](https://github.com/AllureCurtain/oxsum/pull/778) | `8cf07947e98612c2bddca1c13f010b3bfdc369a2` | 2026-11-08 11:47:44 | fix: bind bundle promotions to immutable published offers |
| [#780](https://github.com/AllureCurtain/oxsum/pull/780) | `c79a588455b31e9cc7ceafd9341806a83f1dfc62` | 2026-11-08 12:19:14 | fix(core): bind plan period payment to the customer's sold quote |

## Integrated CI is not green

[CI run 42446328206](https://github.com/AllureCurtain/oxsum/actions/runs/42446328206) is for the inspected SHA. Formatting, Clippy, lib/doc tests, frontend, WASM, publishability, test archive build and test-extras passed. The three integration shards failed and stopped remaining tests early:

- Shard 1: `bundle_upgrade::upgrade_retains_fulfilled_history_and_fences_pending_bundles_and_old_writers` compares the pre-upgrade row with a post-upgrade object that additionally contains `plan_payment: null`.
- Shard 2: `bundles::published_bundle_pins_grants_retirement_expiry_and_replay` fails an unchanged order replay with `Conflict("payment order terms changed for the same idempotency key")`.
- Shard 3: `bundles::retirement_races_new_orders_without_rewriting_any_admitted_offer` fails the same replay invariant.

The replay path directly compares the request's zero/default promotion assertion with the published offer's pinned nonzero promotion. This is a behavior regression, distinct from the upgrade snapshot failure. [#793](https://github.com/AllureCurtain/oxsum/issues/793) owns their correction and the related historical collection snapshot checks. This checkpoint does not mark those repairs passed. Earlier branch-local test counts cannot be transferred to the merged tree. The failure list is incomplete; subsequent review found that #784 statement-prepayment seeding conflicts with #776 closing application. [#795](https://github.com/AllureCurtain/oxsum/issues/795) repairs the repeated partial-prepayment application and missing full-payment transition, with quote-backed fixtures that keep renewal separate from old-debt repayment. No unexecuted downstream test is counted as passed.

The last observed successful candidate workflow is [run 42338752086](https://github.com/AllureCurtain/oxsum/actions/runs/42338752086), SHA `813c788613f9f2a737562493c3b305ca1aaff5c1`, from the preceding stage. No operator-launch candidate has been frozen or certified.

## Remaining work is explicit

- [Financial conservation](plan-refund-conservation.md): #784 integrated partial statement prepayment and void synchronization repairs; the combined #776/#784 prepayment regression is tracked by [#795](https://github.com/AllureCurtain/oxsum/issues/795). H1 settled-usage re-billing, H2 unconsumed mid-period refund notes and H3 excess-refund cash allocation remain confirmed defects with ignored reproductions. N11 daily-recognition policy exists only as an adopted decision; code, migrations and jobs still need implementation. #777 explicit scoped statement repayment remains open.
- [Provider administration](business-providers.md): connection/secrets are implemented; model/alias/cache/fallback/routing editing and browser checks remain.
- [Merchant readiness](hyperswitch-sandbox.md): the pinned local switch, persistence and dummy-connector checks passed. Merchant UI/checkout selection and public HTTPS SDK/automatic callback acceptance remain. Locally signed returned switch objects do not constitute genuine PSP certification.
- N09 readiness still uses the legacy admin-token, positive-wallet and live-report-link checklist; actor/entitlement readiness and immediate first-turn evidence remain.
- [Embedded provisioning](integration-scopes.md) and [billing reads](integration-billing-read.md) are integrated. Key/entitlement/checkout/evidence operations, complete account/application lifecycle, human linking, notifications and an external application walkthrough remain.
- [Candidate tooling](candidate-format.md) validates byte/provenance inputs. The deploy workflow still defaults to a tag and couples publication to deployment; immutable promotion/deployment and rollback rehearsal remain.
- New browser journeys, N12 candidate offline exports, N16 reference capacity/eight-hour soak, N17 diagnostics/upgrade/restore, N18 exact-artifact acceptance and N19 pilot kits remain. Suitable reference hardware, genuine PSP credentials and two independent participants affect only their corresponding external gates. Local mail capture is sufficient for invitation acceptance.

## Workspace and budget observation

The main checkout and these six retained worktrees had no tracked modifications: `business-profile`, `business-providers`, `candidate-artifact`, `integration-scopes`, `operator-launch-contract` and `owner-browser-setup`. Their original task changes are integrated; verify ownership/dependents before lifecycle cleanup. Active repair/reconciliation worktrees are recorded in the execution ledger.

The shared provider-budget file still records an empty attempt list, zero reserved and actual CNY millionths, a CNY 0.10 ceiling and five-attempt maximum. This review made no provider, payment or external mail request and did not alter the budget.

## How the snapshot was checked

GitHub CLI reads inspected merged/open PRs, issue states, the exact CI jobs and latest candidate run. Git status/worktree/log checks established local identity and retained worktree state. Source and development evidence confirmed remaining implementation and acceptance boundaries. The reconciliation PR checks issue uniqueness, complete N/R/J coverage, local document links, merge-commit ancestry and whitespace; it does not rerun application tests.

Historical evidence files retain their original branch SHAs, pending review statements and validation scope. Read [EXECUTION.md](../EXECUTION.md) for the current status rather than treating an older evidence checkpoint as a live queue.
