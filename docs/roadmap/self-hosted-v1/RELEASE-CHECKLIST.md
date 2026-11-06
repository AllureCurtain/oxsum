# Release checklist — promotion gates and external actions

The G1–G5 promotion gates from [ACCEPTANCE.md](ACCEPTANCE.md), each recorded
as pass / fail / external-pending with its evidence pointer, followed by the
exact external-action list T31 executes. Nothing on this page is a
production-ready claim; G4 and G5 are unexercised by policy.

## Promotion gates

| Gate | Contract | Verdict | Evidence |
| --- | --- | --- | --- |
| G1 — T00–T30 software validated and integrated; no unresolved money/permission/isolation correctness issue | All task PRs merged through normal green checks; A01–A30 matrix green; no open correctness defect | **pass** | [STATUS.md](STATUS.md) task table (T00–T30 integrated, 38 PRs); [ACCEPTANCE-RESULTS.md](ACCEPTANCE-RESULTS.md) — every A-row cites its proving suite/script; no unresolved money, permission or isolation defect is open |
| G2 — exact promoted SHA passes relevant full checks, built UI and independent packaged verifier | Candidate gate on one SHA | **pass (software)** | `release-candidate.yml` on main (`45e4b22`): same-SHA resolve → fast tier → wasm → frontend → `check-verify-consumer.sh` → `verify-examples.sh` → no-push image → `candidate-bundle`. The *run* is a maintainer `workflow_dispatch` (gate 1 in [docs/self-hosted-release.md](../../self-hosted-release.md)); this checklist records the gate exists, landed and is runnable — its first real candidate run is T31's dispatch |
| G3 — measured capacity and fault/upgrade/restore evidence; accurate support matrix | D09 workload, soak, upgrade/restore, support matrix | **pass with declared limitation** | [evidence/T27.md](evidence/T27.md) (17 fault cells, schema-count/fairness/cache measurements, harness hazards); `install_ops.rs` 7/7 (upgrade fixture, cross-database restore); [docs/integrations.md](../../integrations.md) + [docs/unsupported.md](../../unsupported.md) support/refusal matrices. **Limitation:** the normalized D09 shape (1,000 customers / 100 streams / 4 vCPU target) and the 5-minute soak are unmeasured — `scripts/soak.sh` exists, unrunnable to declared target locally. Recorded as a release limitation |
| G4 — primary live collection certification and independent pilots with authorized real evidence | Real money + real outsiders | **external-pending** | Requires authorized merchant credentials, real-provider paid calls and contact-authorized pilot operators — enumerated under *External actions* below |
| G5 — user authorizes actual publication/deployment for the reviewable candidate | Separate human authorization | **external-pending** | The candidate is reviewable (`candidate-bundle` per run, this checklist, ACCEPTANCE-RESULTS). Publication (`release.yml` version tag, npm publish, deploy) requires a separate explicit user authorization — never implied by G1–G3 |

## External actions (T31's work list)

Every item below requires an explicit authorization or credential that does
not exist inside the repository. Each names its prerequisite and the
evidence it must produce. None may be simulated or marked complete from
in-repo artifacts.

1. **Merchant sandbox + low-value live payment/refund certification**
   - *Prerequisite:* user-supplied merchant credentials (Stripe/Airwallex/
     Hyperswitch sandbox + live, per [docs/rails.md](../../rails.md)) and
     specific authorization for real-money amounts and recipients.
   - *Must produce:* recorded sandbox and low-value live payment + refund
     cycles (order → provider event → fulfillment → refund → credit note)
     with the operator, timestamp, amount and transaction references —
     secrets and personal data excluded.
   - *Covers:* A16–A18 external halves, A31 primary collection.

2. **Real-provider validation of the supported certified examples**
   - *Prerequisite:* provider credentials for the supported model providers
     ([docs/integrations.md](../../integrations.md) matrix) and paid-call
     authorization.
   - *Must produce:* each supported example's real-provider run (metered
     call → usage/bill → evidence bundle → offline verify) with model,
     cost and outcome recorded.
   - *Covers:* A15/A27 external halves.

3. **Two independent pilot operators**
   - *Prerequisite:* user authorization to contact real outside operators,
     and the operators themselves.
   - *Must produce:* two independent install + billing/plan-cycle
     completions — each pilot's environment, install path, plan sale and
     billing cycle result, who performed it and when, documented without
     secrets or personal data (per T31's "never simulate outside-user
     evidence" rule).
   - *Covers:* A31 pilots; exercises [ONBOARDING-DRYRUN.md](ONBOARDING-DRYRUN.md)
     end to end on unfamiliar infrastructure.

4. **Publication and live deployment**
   - *Prerequisite:* separate user authorization after the exact candidate
     is reviewable — the `candidate-bundle` from a `release-candidate.yml`
     run on the chosen SHA, this checklist and ACCEPTANCE-RESULTS.
   - *Must produce:* `gh workflow run release.yml -f image_tag=v<x.y.z>`,
     npm publication of `verify-oxsum-bill`, and (separately authorized)
     `deploy.yml` — each a distinct recorded decision, never bundled into
     implementation work.
   - *Covers:* G5.

5. **Candidate gate run on the final SHA** (software, but gated on
   authorization to run the workflow)
   - *Prerequisite:* none beyond maintainer access — but the run belongs to
     the release decision, not this acceptance.
   - *Must produce:* `candidate-bundle` (manifest + SHA256SUMS + notes
     draft) from `gh workflow run release-candidate.yml -f sha=<chosen>`,
     plus a `scripts/release-candidate-smoke.sh` green run against that
     build.
   - *Covers:* the executed half of G2/A30.

## Standing non-claims

- No live payment rail has moved real money — every payment test ran the
  stub/sandbox path.
- No independent third party has installed or operated this software.
- Nothing has been published to a registry or deployed to a server as a
  release.
- `verify-oxsum-bill` remains unpublished on npm (`npm view` answers 404);
  the packed-tarball path is the verified distribution.
