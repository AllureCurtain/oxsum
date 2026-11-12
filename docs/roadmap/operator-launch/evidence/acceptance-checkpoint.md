# Acceptance checkpoint — 2026-11-12

Issue #984. GitHub main is `d4232c7d`.
[Verified merge identities](verified-merges.json) now include every previously
pending PR through #1029. Implementation integration, development acceptance and
exact-artifact release acceptance remain separate. N00/N01/N02/N07/N08 owner
issues are closed; the other owner issues and umbrella #725 stay open only for
their remaining journey/external/candidate gates, not for pending software
integration. This ratio is not software completion.

## Integrated software and development evidence

Application lifecycle/keys/entitlement/checkout/evidence, personal consent, signed
notifications and durable external application are integrated through #943/#949.
Readiness, staff invitations, console reload, exact plan amounts and first-call
WASM repairs are integrated through #967. Owner/customer/staff, bundle purchase/
return and paid-plan activation/cancellation evidence is integrated through #974.
Aggregate integration binaries and shared-target rules integrated in #1001/#1000.

Finance's retained [source/ledger comparison](browser-financial-comparison/README.md)
verified 15 sources and 20 entries with all six monetary totals equal. Four
missing-cost exceptions remain open; no invented cost or waiver supplies evidence.
Quota exhaustion/overage and two-customer/pinned in-flight price acceptance now
have separate evidence in merged PRs #991/#995. Monthly credit/statement/
issuance/repayment operations are integrated in #1005/#1008/#1010/#1012/#1014/#1016
with staff browser evidence on the merged heads.

## Previously pending — all merged

The November 11 checkpoint recorded **25 open PRs** awaiting protected
integration. As of 2026-11-12 **all 25 are merged** onto main:

| PR | Established | Merge commit | Merged |
| --- | --- | --- | --- |
| #975 | Historical manual refund upgrade fixture | `2cc680ee` | 2026-11-11 |
| #977 | Application consent and signed durable delivery/replay | `e92ac2a4` | 2026-11-11 |
| #979 | Original quota-window consumption | `99d9ead7` | 2026-11-11 |
| #980 | Explicit dispute outcomes and retained exact retry | `5534c2ee` | 2026-11-11 |
| #982 | Paid journal correction and original-source returns | `3af0951d` | 2026-11-11 |
| #985 | Current integration and Finance comparison evidence | `5b73501f` | 2026-11-11 |
| #987 | ISO reconciliation dates | `b5ca0260` | 2026-11-11 |
| #989 | Explicit complimentary binding and ambiguous recovery | `048e80f1` | 2026-11-11 |
| #991 | Browser quota boundary and offline proof | `99a233e7` | 2026-11-11 |
| #993 | Chat credential isolation across user/customer | `f66c4f8a` | 2026-11-11 |
| #995 | Distinct customer and in-flight sale prices | `154862d6` | 2026-11-11 |
| #996 | Bilingual sale editor and exact validation | `742294f7` | 2026-11-11 |
| #1004 | Aggregate CI/process-shared suite execution | `98e95342` | 2026-11-11 |
| #1005 | Staff monthly credit terms | `75005313` | 2026-11-11 |
| #1008 | Staff monthly statement reads | `8b1e6895` | 2026-11-11 |
| #1010 | Atomic monthly issuance/anchor | `9ffd411d` | 2026-11-11 |
| #1012 | Reviewed staff monthly issuance | `a40c9574` | 2026-11-11 |
| #1014 | Atomic monthly repayment/paid replay | `9a0171e8` | 2026-11-11 |
| #1016 | Staff monthly verified payment | `5a75f3f7` | 2026-11-11 |
| #1020 | Separate manual renewal from overdue-debt guidance | `8a83ea20` | 2026-11-11 |
| #1021 | Readable mobile statement amounts | `5b73839e` | 2026-11-11 |
| #1022 | Policy-2 renewal/change/cancel/expiry browser evidence | `f9e1bc49` | 2026-11-11 |
| #1025 | Provider rotation/repoint and pinned SSE evidence | `3e69a4e9` | 2026-11-11 |
| #1026 | Actual gateway status in request logs after admission | `99435ba9` | 2026-11-11 |
| #1029 | Exact partial plan dispute amounts and authenticated-source comparison | `0b4df4ae` | 2026-11-11 |

Post-merge PR #1030–#1057 work (invoice reissue, Stripe refund recovery, sale
gating/effective-window fixes, models-sale gating, fallback 4xx, README/docs
refresh) is also merged on main through `d4232c7d`. Current main CI health must
be read from the newest main run rather than the older `2ee70601` selector
failure, which PR #1004 repaired.

## Remaining gates — unchanged

- **External prerequisites still unperformed**: genuine PSP sandbox certification
  completion (Stripe test-mode capture→webhook→credited is certified 2026-11-11;
  Airwallex hosted-page capture/webhook remains provider-side blocked), secure
  SDK/HTTPS automatic callback path, native Linux amd64 4-CPU/8-GiB capacity and
  eight-hour soak host, and two independent operator participants. Local macOS
  fixtures cannot replace these gates. Pilot preparation is integrated in #874.
- **Exact-candidate acceptance**: candidate `cc16bb8f` is frozen with green
  full-tier CI [42951039071](https://github.com/AllureCurtain/oxsum/actions/runs/42951039071)
  and all nine manifest gates. J01–J16/J18 acceptance against the built image —
  portal/embedded/offline/deploy/restore journeys — remains. No final
  operator-launch candidate has completed acceptance.
- **Conservation umbrellas**: #768, #779 and #829 stay open. Their child repairs
  are integrated (#778/#780/#784/#798/#812/#815/#837/#840→#847/#841→#868/#842→#903
  and #1029); what remains is the explicit acceptance audit each umbrella
  requires, plus any residual scope the audit surfaces — not merged software.
- **Browser acceptance**: #946 and #969 stay open for the remaining commercial,
  embedded and final-candidate journeys. Development browser evidence already
  integrated does not substitute for candidate acceptance.
- **Public publication and existing-service upgrades remain unperformed** under
  the recorded scope; tooling and isolated rehearsal do not claim those actions.

## Persistent allowance and cleanup

DeepSeek allowance remains **2/5 attempts**, **16,896 CNY millionths reserved**,
**52 millionths** estimated usage (candidate certification consumed attempts
3–4/5; the 4/5 recorded mid-checkpoint was the reservation state at that write).
No new real-provider request occurred during monthly or reconciliation work.
Historical 0/5 entries are superseded, never a reset.

The private cleanup journal retained 67 explicitly removed merged worktrees. This
checkpoint reconciles a further **13 merged worktrees removed on 2026-11-12**
(`bundle-refund-upgrade`, `chat-credential-isolation`,
`complimentary-plan-binding`, `customer-credit-terms`, `dispute-outcomes`,
`embedded-browser-evidence`, `launch-status`, `paid-journal-void`,
`plan-consumption-read`, `quota-browser`, `reconciliation-dates`, `sale-browser`,
`sale-locales` — all clean, zero unpushed commits) plus **9 superseded local
branches** whose content is already integrated on main (`build/candidate-*`,
`build/verify-lock-dependency`, `build/audit-postgres-dependencies`,
`ci/verified-*`, `backup/funded-call-admission-before-ledger-merge`). One remote
branch remains: `fix/staff-invitation-preview` (unmerged; the staff-preview
repair it carried was superseded by merged #989/#993 work — verify before
deleting). The shared build cache is not a task checkout.
