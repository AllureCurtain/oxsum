# Release status

Updated: 2026-11-05. Contract issue: [#591](https://github.com/AllureCurtain/oxsum/issues/591).

Implementation: in progress. Release: not ready. T00 is integrated; T01.a and T02 are in review; T01.b is locally validated. No release readiness claimed.

| Task | Issue | State | Depends on |
| --- | --- | --- | --- |
| T00 — Adopt the release contract and baseline | [#591](https://github.com/AllureCurtain/oxsum/issues/591) | integrated (PR #623) | None |
| T01 — Restrict financial and metering authority | [#592](https://github.com/AllureCurtain/oxsum/issues/592) | in_progress | T00 |
| T02 — Add business and customer ownership without moving ledgers | [#593](https://github.com/AllureCurtain/oxsum/issues/593) | in_review (PR #627) | T00 |
| T03 — Business staff, setup and session contexts | [#594](https://github.com/AllureCurtain/oxsum/issues/594) | pending | T01, T02 |
| T04 — Customer profile and team lifecycle | [#595](https://github.com/AllureCurtain/oxsum/issues/595) | pending | T02, T03 |
| T05 — Scoped consumption and integration credentials | [#596](https://github.com/AllureCurtain/oxsum/issues/596) | pending | T03, T04 |
| T06 — Business-owned provider configuration | [#597](https://github.com/AllureCurtain/oxsum/issues/597) | pending | T02, T03 |
| T07 — Model catalog and private serving context | [#598](https://github.com/AllureCurtain/oxsum/issues/598) | pending | T05, T06 |
| T08 — Versioned customer sale books | [#599](https://github.com/AllureCurtain/oxsum/issues/599) | pending | T02, T04, T06 |
| T09 — Bind sale terms into evidence | [#600](https://github.com/AllureCurtain/oxsum/issues/600) | pending | T08 |
| T10 — Durable request operation | [#601](https://github.com/AllureCurtain/oxsum/issues/601) | pending | T05, T07, T08 |
| T11 — Transaction-aware ledger effects and zero-cost receipts | [#602](https://github.com/AllureCurtain/oxsum/issues/602) | pending | T10 |
| T12 — Coordinated customer admission and settlement | [#603](https://github.com/AllureCurtain/oxsum/issues/603) | pending | T09, T10, T11 |
| T13 — Cover every serving and metering path | [#604](https://github.com/AllureCurtain/oxsum/issues/604) | pending | T12 |
| T14 — Merchant settings and scoped event intake | [#605](https://github.com/AllureCurtain/oxsum/issues/605) | pending | T03, T04 |
| T15 — Payment orders and durable fulfillment | [#606](https://github.com/AllureCurtain/oxsum/issues/606) | pending | T11, T14 |
| T16 — Customer checkout and purchased funding | [#607](https://github.com/AllureCurtain/oxsum/issues/607) | pending | T04, T05, T15 |
| T17 — Corrections, partial refunds and disputes | [#608](https://github.com/AllureCurtain/oxsum/issues/608) | pending | T12, T16 |
| T18 — Promotional lots and prepaid bundles | [#609](https://github.com/AllureCurtain/oxsum/issues/609) | pending | T12, T15 |
| T19 — Customer entitlement and paid plan lifecycle | [#610](https://github.com/AllureCurtain/oxsum/issues/610) | pending | T12, T15, T18 |
| T20 — Plan fee journals and revenue allocation | [#611](https://github.com/AllureCurtain/oxsum/issues/611) | pending | T17, T19 |
| T21 — Customer credit, statements and overdue handling | [#612](https://github.com/AllureCurtain/oxsum/issues/612) | pending | T04, T12, T17, T20 |
| T22 — Financial facts, cost coverage and reconciliation | [#613](https://github.com/AllureCurtain/oxsum/issues/613) | pending | T13, T17, T20, T21 |
| T23 — Customer self-service portal | [#614](https://github.com/AllureCurtain/oxsum/issues/614) | pending | T04, T05, T09, T13, T16, T17, T19, T21 |
| T24 — Business console and operational resolution | [#615](https://github.com/AllureCurtain/oxsum/issues/615) | pending | T03, T04, T06, T08, T17, T21, T22 |
| T25 — End-to-end customer evidence and compatibility | [#616](https://github.com/AllureCurtain/oxsum/issues/616) | pending | T09, T13, T17, T20, T21 |
| T26 — Supported integrations and developer examples | [#617](https://github.com/AllureCurtain/oxsum/issues/617) | pending | T05, T13, T15, T19, T25 |
| T27 — Cross-cutting fault, isolation and capacity hardening | [#618](https://github.com/AllureCurtain/oxsum/issues/618) | pending | T13, T17, T18, T19, T20, T21, T22, T25 |
| T28 — Self-hosted installation, upgrade and restore | [#619](https://github.com/AllureCurtain/oxsum/issues/619) | pending | T02, T03, T14, T20, T21 |
| T29 — Release artifact and candidate gates | [#620](https://github.com/AllureCurtain/oxsum/issues/620) | pending | T25, T26, T28 |
| T30 — Final integrated acceptance and launch materials | [#621](https://github.com/AllureCurtain/oxsum/issues/621) | pending | T23, T24, T26, T27, T28, T29 |
| T31 — External certification and promotion gates | [#622](https://github.com/AllureCurtain/oxsum/issues/622) | pending | T30 |

## Current checkpoint

T00 merged through [PR #623](https://github.com/AllureCurtain/oxsum/pull/623), commit `aadde114ca61cf97d5254ae3ea45d0a18e4542ba`. [CI](https://github.com/AllureCurtain/oxsum/actions/runs/41869652216) passed relevant documentation gates; heavy suites were skipped by path policy. Local and GitHub main synchronized.

T01 split: [#624](https://github.com/AllureCurtain/oxsum/issues/624) is PR #626, CI green but Entire requires independent approval; [#625](https://github.com/AllureCurtain/oxsum/issues/625) removes raw customer holds/settlements. Both are required. T01.b is implemented on the validated T01.a branch, in `.worktrees/customer-billing-authority`, `fix/customer-billing-authority`; normal integration waits for T01.a approval. See [T01.a evidence](evidence/T01-a.md) and [T01.b evidence](evidence/T01-b.md).

T02 is independently implemented and locally validated in PR #627 (`d5b3c0f`), with CI green. Entire reports only missing independent approval, no blocking findings. It is not integrated; preserve migrations 0071 and 0072 and revalidate the combined candidate during integration. No approval bypass is authorized.


## External gates

- Merchant sandbox/live certification: pending merchant credentials and specific transaction authorization.
- Real paid provider certification: pending credentials and paid-call authorization.
- Two independent pilots: pending actual operators and contact authorization.
- Publication/live deployment: pending exact validated candidate and separate user authorization.

## Evidence

Use the fields in [ACCEPTANCE.md](ACCEPTANCE.md). No skipped database suite counts as passed. Update this file and task evidence at every checkpoint; preserve old evidence when superseded.
