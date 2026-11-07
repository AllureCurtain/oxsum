# Release checklist — promotion gates and external actions

Updated 2026-11-07. Candidate:
`813c788613f9f2a737562493c3b305ca1aaff5c1`. See the pinned evidence in
[closeout-candidate.md](evidence/closeout-candidate.md); documentation added
after that commit does not change the candidate identity. No publication or
production-ready status is implied.

## Promotion gates

| Gate | Verdict | Evidence and limit |
| --- | --- | --- |
| G1 — T00–T30 software validated/integrated; no unresolved money/permission/isolation defect | integrated, with scoped acceptance | [STATUS.md](STATUS.md) maps merged work. #686/#691/#693/#694 fix the concrete closeout defects. Automated and browser evidence is scoped in [ACCEPTANCE-RESULTS.md](ACCEPTANCE-RESULTS.md); no blanket A01–A30 certification. |
| G2 — exact promoted SHA full checks, built UI and independent package | executed candidate evidence | [closeout-candidate.md](evidence/closeout-candidate.md): full CI, mandatory consumer, image startup and version SHA match, no-push image archive/manifest/checksums. Interactive browser evidence used incremental local builds, not a complete replay against this image. |
| G3 — measured capacity/fault/upgrade/restore and accurate support matrix | measured; performance target not met | [closeout-capacity.md](evidence/closeout-capacity.md): 1,000 accounts/10,000 keys/100 streams, 5,897 successes/zero failures, zero drift. 19.657 admissions/s and 113.421 ms p95 overhead upper bound miss >=20/s and <100 ms. [T27](evidence/T27.md) fault suites and `install_ops.rs` upgrade/restore run in full CI. Four-vCPU certification and long-duration load remain pending. |
| G4 — primary collection certification and independent pilots | external-pending | Real PSP capture/refund, remaining real-provider scenarios and two independent operators have not been established. |
| G5 — explicit publication/deployment authorization | external-pending | Review this exact candidate and its limitations before separately authorizing release/image/npm publication or live deployment. |

The candidate workflow is authorized software validation. It requires no
additional release permission because it exports artifacts without publishing
or deploying. A green workflow is evidence of its checks, not permission to
promote or a waiver of the D09 shortfall.

## External actions

1. **Merchant sandbox and low-value live payment/refund.** Supply working
   credentials and specific authorization for amounts/recipients. Record the
   real order → capture → authenticated provider event → fulfillment →
   provider refund → credit-note cycle with sanitized references. The older
   Airwallex intent creation, manually seeded reference, locally signed event
   and ledger reversal are not capture/refund certification.
2. **Remaining real-provider examples.** Supply the relevant credentials and
   paid-call authorization. The historical DeepSeek nonstreaming call is
   limited evidence; run the remaining advertised protocols, streaming,
   retry/fallback and examples with exported/offline-verified bills.
3. **Two independent operators.** Obtain actual participants and contact
   authorization. Each independently installs and completes a plan/billing
   cycle, recording environment, operator and result. The internal
   [onboarding walkthrough](ONBOARDING-DRYRUN.md) is preparation, not pilot evidence.
4. **Publication/deployment.** Make distinct explicit decisions for the
   reviewed candidate's release/image, npm package and live deployment.
   `release.yml` and `deploy.yml` were not run in this closeout. The verified
   SDK distribution remains the packed tarball.

## Software limitations to carry forward

- Improve or expressly accept the measured D09 throughput/overhead shortfall
  for a narrower preview; do not relabel the thresholds after measuring.
- Complete broader browser coverage for paid plans, disputes and receivables
  before claiming the entire console/portal has been exercised.
- Before promoting a later code SHA, rerun full `ci.yml` and
  `release-candidate.yml` for that SHA; older green artifacts do not transfer.
