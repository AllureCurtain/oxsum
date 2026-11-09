# Integrated software and remaining gates (2026-11-09)

Issue #914 reconciles current-state documentation against GitHub and local main
`5285fc0b3bc825b0707bba675ea5d1ff0efa903d`. The following PRs were verified
`MERGED`; a merged software unit does not imply browser or release acceptance.

| PR | Merge commit | Implemented change |
| --- | --- | --- |
| [#896](https://github.com/AllureCurtain/oxsum/pull/896) | `a60e14ca2274109ccdb87a599df156ddf12d1dff` | feat: reconcile financial sources against verified ledger entries |
| [#897](https://github.com/AllureCurtain/oxsum/pull/897) | `b20934c4f16e7d6b32ad2565a71536abd4c21d69` | fix: stop new quota draws at cancelled plan boundaries |
| [#900](https://github.com/AllureCurtain/oxsum/pull/900) | `54b72b1a7be968390df7ae4a12e9ce256e05c1bc` | feat: issue bounded application customer keys |
| [#902](https://github.com/AllureCurtain/oxsum/pull/902) | `bbbcd154dd70628a796fe1c94991b39961db3c80` | fix: serialize team deletion before admission roster locks |
| [#903](https://github.com/AllureCurtain/oxsum/pull/903) | `070cf4bec8800de4b63e414434b4f5f14cb3fb2d` | fix: compensate proven legacy non-plan chargebacks with exact sources |
| [#904](https://github.com/AllureCurtain/oxsum/pull/904) | `1f00c5bfafa826685c05008145c08c393bf1a88c` | fix: bound gateway evidence and commit complete policy reasons |
| [#839](https://github.com/AllureCurtain/oxsum/pull/839) | `8a51109cec622020ae3cad5aff04658ee243c95e` | test: verify real artifact upgrade restore rollback and retained proofs |
| [#823](https://github.com/AllureCurtain/oxsum/pull/823) | `d17ad874e70f4ecd6a88ca17b946e15413cc807f` | feat(web): manage provider models and routing in business sessions |
| [#834](https://github.com/AllureCurtain/oxsum/pull/834) | `0ea3ef3d20744552830049935c4f53c7269a1dc3` | feat(web): complete business merchant configuration controls |
| [#844](https://github.com/AllureCurtain/oxsum/pull/844) | `23b6547b0c518ea82f494ebe31e626eae729d4a3` | feat: route checkout through explicit business merchant selection |
| [#868](https://github.com/AllureCurtain/oxsum/pull/868) | `f3802d5e0a9631f20eddeb5473e49e3855e27d85` | fix: retain non-plan refund cash and exact historical recovery |

The current-main regression fixtures include #905–#907. Main CI `42596801912`
passed its executed jobs; conditional/skipped engine or release gates are not
silently credited. Existing detailed evidence retains development SHA, tests
and review history.

The remaining software includes #777 named statement repayment, #901 personal
readiness and #747 embedded checkout/evidence, verified linking, notifications
and a runnable independent consumer. #829/#779 need explicit child-to-acceptance
reconciliation before closure. #908/#910/#911/#913 have required CI green and
remain pending independent approval; no merge bypass was used.

Remaining acceptance includes the complete N03–N21 browser/journey matrix,
final candidate full CI/artifact verification and runtime rehearsals, native
Linux amd64 reference capacity plus eight-hour soak, actual bounded provider
calls, genuine PSP sandbox evidence and two actual external pilot participants.
The provider allowance is still 0/5 attempts, CNY 0. No public publication,
external contact, live payment, paid cloud or existing deployment upgrade occurred.

## Authorized workspace cleanup

Thirty-eight merged worktrees were removed after checking merge state and active
dependencies. Their branch references were kept. Active unmerged tasks, detached
regression baselines with retained local edits, and the shared target cache
remain. A deleted worktree's `node_modules` link was repaired with `npm ci` in
the active application-read worktree; dependent frontend links must stay valid
until their work completes. Worktree deletion is not evidence of issue closure.

## Documentation checks

All listed merge identities were retrieved from `gh pr view` and verified
against `origin/main`; current repository links and whitespace were checked.
This PR changes only status/evidence documentation and reuses prior code tests.
