# Release status

Updated: 2026-11-07. Contract issue: [#591](https://github.com/AllureCurtain/oxsum/issues/591).

Implementation: **T00–T30 integrated, with closeout fixes**. Release:
**not promoted**. The closeout corrected archived integration execution,
candidate evidence, session-context handling, payment fulfillment and the
browser journey. Functional checks and external certification are separate;
D09's measured throughput/overhead misses its target. No production-ready
claim is made.

| Task | Issue | State | Depends on |
| --- | --- | --- | --- |
| T00 — Adopt the release contract and baseline | [#591](https://github.com/AllureCurtain/oxsum/issues/591) | integrated (PR #623) | None |
| T01 — Restrict financial and metering authority | [#592](https://github.com/AllureCurtain/oxsum/issues/592) | integrated (PR #626, #628) | T00 |
| T02 — Add business and customer ownership without moving ledgers | [#593](https://github.com/AllureCurtain/oxsum/issues/593) | integrated (PR #627) | T00 |
| T03 — Business staff, setup and session contexts | [#594](https://github.com/AllureCurtain/oxsum/issues/594) | integrated (PR #631, #632) | T01, T02 |
| T04 — Customer profile and team lifecycle | [#595](https://github.com/AllureCurtain/oxsum/issues/595) | integrated (PR #635, #636) | T02, T03 |
| T05 — Scoped consumption and integration credentials | [#596](https://github.com/AllureCurtain/oxsum/issues/596) | integrated (PR #639, #640) | T03, T04 |
| T06 — Business-owned provider configuration | [#597](https://github.com/AllureCurtain/oxsum/issues/597) | integrated (PR #643, #646) | T02, T03 |
| T07 — Model catalog and private serving context | [#598](https://github.com/AllureCurtain/oxsum/issues/598) | integrated (PR #647, #653) | T05, T06 |
| T08 — Versioned customer sale books | [#599](https://github.com/AllureCurtain/oxsum/issues/599) | integrated (PR #652) | T02, T04, T06 |
| T09 — Bind sale terms into evidence | [#600](https://github.com/AllureCurtain/oxsum/issues/600) | integrated (PR #657) | T08 |
| T10 — Durable request operation | [#601](https://github.com/AllureCurtain/oxsum/issues/601) | integrated (PR #656) | T05, T07, T08 |
| T11 — Transaction-aware ledger effects and zero-cost receipts | [#602](https://github.com/AllureCurtain/oxsum/issues/602) | integrated (PR #658) | T10 |
| T12 — Coordinated customer admission and settlement | [#603](https://github.com/AllureCurtain/oxsum/issues/603) | integrated (PR #660) | T09, T10, T11 |
| T13 — Cover every serving and metering path | [#604](https://github.com/AllureCurtain/oxsum/issues/604) | integrated (PR #662) | T12 |
| T14 — Merchant settings and scoped event intake | [#605](https://github.com/AllureCurtain/oxsum/issues/605) | integrated (PR #650, #655) | T03, T04 |
| T15 — Payment orders and durable fulfillment | [#606](https://github.com/AllureCurtain/oxsum/issues/606) | integrated (PR #659) | T11, T14 |
| T16 — Customer checkout and purchased funding | [#607](https://github.com/AllureCurtain/oxsum/issues/607) | integrated (PR #663) | T04, T05, T15 |
| T17 — Corrections, partial refunds and disputes | [#608](https://github.com/AllureCurtain/oxsum/issues/608) | integrated (PR #665) | T12, T16 |
| T18 — Promotional lots and prepaid bundles | [#609](https://github.com/AllureCurtain/oxsum/issues/609) | integrated (PR #664) | T12, T15 |
| T19 — Customer entitlement and paid plan lifecycle | [#610](https://github.com/AllureCurtain/oxsum/issues/610) | integrated (PR #661) | T12, T15, T18 |
| T20 — Plan fee journals and revenue allocation | [#611](https://github.com/AllureCurtain/oxsum/issues/611) | integrated (PR #666) | T17, T19 |
| T21 — Customer credit, statements and overdue handling | [#612](https://github.com/AllureCurtain/oxsum/issues/612) | integrated (PR #667) | T04, T12, T17, T20 |
| T22 — Financial facts, cost coverage and reconciliation | [#613](https://github.com/AllureCurtain/oxsum/issues/613) | integrated (PR #670) | T13, T17, T20, T21 |
| T23 — Customer self-service portal | [#614](https://github.com/AllureCurtain/oxsum/issues/614) | integrated (PR #668) | T04–T21 surface |
| T24 — Business console and operational resolution | [#615](https://github.com/AllureCurtain/oxsum/issues/615) | integrated (PR #673) | T03–T22 surface |
| T25 — End-to-end customer evidence and compatibility | [#616](https://github.com/AllureCurtain/oxsum/issues/616) | integrated (PR #671) | T09–T21 evidence |
| T26 — Supported integrations and developer examples | [#617](https://github.com/AllureCurtain/oxsum/issues/617) | integrated (PR #674) | T05–T25 |
| T27 — Cross-cutting fault, isolation and capacity hardening | [#618](https://github.com/AllureCurtain/oxsum/issues/618) | integrated (PR #672) | T13–T25 |
| T28 — Self-hosted installation, upgrade and restore | [#619](https://github.com/AllureCurtain/oxsum/issues/619) | integrated (PR #669) | T02–T21 ops |
| T29 — Release artifact and candidate gates | [#620](https://github.com/AllureCurtain/oxsum/issues/620) | integrated (PR #675, #687) | T25, T26, T28 |
| T30 — Final integrated acceptance and launch materials | [#621](https://github.com/AllureCurtain/oxsum/issues/621) | integrated (PR #676; closeout #695) | T23–T29 |
| T31 — External certification and promotion gates | [#622](https://github.com/AllureCurtain/oxsum/issues/622) | external-pending | T30 |

## Acceptance

[ACCEPTANCE-RESULTS.md](ACCEPTANCE-RESULTS.md) separates executed automated
checks, scoped browser journeys, capacity measurements and external evidence.
The earlier blanket A01–A30 pass was invalid: candidate `930794c` had failed
full CI, the old read-only soak mislabeled its extrema as percentiles, and
five screenshots did not exercise payment or staff workflows.

Closeout PRs: [#686](https://github.com/AllureCurtain/oxsum/pull/686)
(archived tests), [#687](https://github.com/AllureCurtain/oxsum/pull/687)
(candidate gating/image startup), [#689](https://github.com/AllureCurtain/oxsum/pull/689)
(capacity/conservation), [#691](https://github.com/AllureCurtain/oxsum/pull/691)
(session shell), [#693](https://github.com/AllureCurtain/oxsum/pull/693)
(payment worker), [#694](https://github.com/AllureCurtain/oxsum/pull/694)
(browser defects/evidence). Final candidate evidence is in
[evidence/closeout-candidate.md](evidence/closeout-candidate.md).

## Remaining release gates and limitations

- D09 measured 5,897 successful requests, zero failures/drift and 100 peak
  streams across 1,000 accounts/10,000 keys. Admission was 19.657/s and p95
  overhead upper bound 113.421 ms: the >=20/s and <100 ms targets were not
  achieved. Four-vCPU and long-duration capacity are not certified.
- Browser evidence covers the listed customer/payment/refund/staff paths;
  it does not certify every plan, dispute or receivables UI workflow.
- Merchant capture/refund certification needs working credentials and
  separately authorized real-money transactions. Local signed callbacks
  and ledger reversal do not prove PSP money movement.
- One historical DeepSeek nonstreaming call passed; remaining supported
  real-provider protocols and examples still need certification.
- Two independent pilots, publication/live deployment and npm publication
  remain pending. Candidate validation and no-push artifact preparation are
  authorized software work, not publication.

## Evidence

Use the fields in [ACCEPTANCE.md](ACCEPTANCE.md). No skipped database suite
counts as passed. Per-task evidence lives in `evidence/`; the integrated
matrix is `ACCEPTANCE-RESULTS.md` and promotion decisions are in
[RELEASE-CHECKLIST.md](RELEASE-CHECKLIST.md).
