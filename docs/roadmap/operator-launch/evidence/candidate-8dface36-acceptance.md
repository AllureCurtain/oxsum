# N18 candidate re-freeze and acceptance re-run — `8dface36` (2026-11-15)

Issue [#744](https://github.com/AllureCurtain/oxsum/issues/744) (N18). Supersedes
the stale 2026-11-11 candidate `cc16bb8f` (schema 0131) as the candidate under
acceptance. This is candidate-image / local-fixture evidence on an isolated Linux
host. It is **not** external certification, publication or deployment authority.

## Candidate identity

- Source: `8dface36bc581c7f91b7b55566e1522680981d9d` (main, merge of #1220; the
  only difference from `8cca80a2` is `TODO.md` and `EXECUTION.md`).
- Full-tier CI on the exact SHA:
  [43665157037](https://github.com/AllureCurtain/oxsum/actions/runs/43665157037)
  (`workflow_dispatch` on main, because the docs-only push was path-ignored).
  All jobs passed, including the three test shards, `test-extras`
  (process-shared and Postgres engine conformance) and `solana-e2e`.
  `scripts/check-candidate-ci.py` accepted it.
- Candidate gate: [release-candidate 43666706400](https://github.com/AllureCurtain/oxsum/actions/runs/43666706400),
  attempt 1, workflow revision `8dface36…`, `gatedAt` 2026-11-15T02:27:19Z.
  All nine gates are `passed` (fmt, clippy, libDocTests, wasm, frontend,
  consumer, examples, image, smoke). The workflow builds with `push: false`. It
  does no registry login, no npm publish and no deploy.
- Version-2 bundle (`candidate-bundle` artifact):
  - `candidate_bundle.py validate`: offline PASS and `--online` PASS (GitHub
    provenance plus byte comparison).
  - Image `configId` is `sha256:026d55cf537df51679991e14a3e4ad355093c38b67124cc0a7fbb8d176b5b1b5`
    on `linux/amd64`. After `docker load`, the image ID equals the `configId`,
    and the OCI revision label is `8dface36…`.
  - SDK is `verify-oxsum-bill@0.1.0`, `sha512-fhXXFwBHpeM+7QszWB2kgmroi94a7kF/Qj8hCK/GxEAl9kzcY2C7VbVfdHy3uoLrYZqmIxve40L3pNpRcEUW9Q==`
    (tarball sha256 `4720ef72…`).
  - `runtimeVersion` reports `{gitSha 8dface36…, version 0.1.0, schemaVersion 0139_solana_refund_signature_uniqueness}`.
- Release notes draft: `RELEASE-NOTES-DRAFT.md` in the bundle (contract 0.131.0).
- No git tag was created and no release, deploy or SDK workflow was run. The
  candidate is recorded by the gated workflow run and this document.
- Rollback baseline only (not a candidate): release-candidate
  [43665738496](https://github.com/AllureCurtain/oxsum/actions/runs/43665738496)
  on `8cca80a2`. It is a distinct, same-schema (0139) image used for the
  compatible-rollback steps below.

## Environment

- Host: Linux x86_64 (Debian 13), 8 vCPU, about 15 GiB RAM, Docker 28.5.1 with
  Compose v2.40.3. The image runs natively as amd64, without emulation. This is
  **not** the declared J15 reference host and certifies no capacity.
- Stacks use `deploy/docker-compose.core.yml` from the candidate source:
  - `oxsum-fresh` for J01–J13/J18, on a fresh PostgreSQL 17 database.
  - `oxsum-accept` for J14.
- Mail is local Mailpit only, and every recipient is a `.test` address.
- The provider is a task-owned loopback OpenAI-protocol stub.
- The PSP is a task-owned Hyperswitch-protocol stub. Payment and refund events
  are explicit local injections signed with the fixture secret (HMAC-SHA512,
  `x-webhook-signature-512`).
- No real provider, PSP, paid model budget, external inbox, registry or
  production host was contacted. DeepSeek allowance is unchanged; no new
  attempt was made.

## Journey results

| Journey | Verdict on `8dface36` | Evidence |
| --- | --- | --- |
| J01 fresh install | pass (UI, local) | Fresh DB, `oxsum-migrate` reached 0139, and `/readyz` returned `ready (0139_solana_refund_signature_uniqueness)`. An admin setup token was minted, then the owner registered through the real `/app/setup` page in headless Chrome. Calls: `POST /business/setup/register` 200, and the page reported the business initialized with invite-only registration. |
| J02 owner setup / console | pass (API + UI spot check) | Business context selected. The business console and its Setup, Customers, Refunds, Statements, Reports, Channels and Plans sections rendered for the owner with zero page errors. |
| J03 channel + rotation | pass | Loopback channel saved under an internal destination policy, and credential BBBB added. AAAA (`…1111`) set to `disabled`. Every served model call in the stub log carried bearer `…BBBB2222`. |
| J04 price / plan | pass (API) | Price v1 for `accept-chat` published (1.5/2.5 cr per M tokens, `maxOutputTokens` 32, `costPerRequest` 40). Plan version published (4 cr fee, 100-minor quota, anniversary). Collection terms moved to revision 2 (USD 1:1, source `owner`). |
| J05 checkout (fixture PSP) | pass (local fixture) | Wallet top-up order went `provider_pending`, then `fulfilled` on signed `payment_succeeded`. Inbox rows `evt_accept_pay_0001/0003/0101/0102` are `applied`. Real PSP stays under N15. |
| J06 customer invitation | pass | Invitation mailed through local SMTP and received in Mailpit. Customer redeemed it and was shown the first key once. Replay of the spent token returned 404. |
| J07 staff roles / revocation | pass; #1072 fixed | Finance and Support were invited by mail. Finance got 200 on financial-facts and merchants; Support got 403 on both. After revocation, the UI lands on "No accessible organization … access was revoked" with a sign-out option and no login loop. The same script on `cc16bb8f` reproduced the `/app` to `/app/login` bounce. |
| J08 call, settlement, evidence | pass (stub provider) | A customer SSE call returned 200 and settled 61 minor (9 in / 3 out tokens). The `/api/v1/entries` usage row links the request, and `/api/v1/usage` attributes 61 minor to `accept-loopback`. Real-provider J08 was not re-run: no paid budget is used in this run. |
| J09 refund recovery | pass (local fixture); review flagged | 20 cr top-up fulfilled, then the customer requested a refund of the whole remainder (20,000,000 minor). Staff approval left it `provider_pending`. A signed `refund_succeeded` made it `succeeded` with a credit note. Because 61 minor had already been consumed, `cashEffect` = cash 20,000,000 / wallet 19,999,939 / **suspense 61**, `requiresReview: true` (`originalWalletFundingNotRetained`). This is the documented non-plan refund policy. The `cc16bb8f` run refunded only the remaining balance, so it never hit this branch. |
| J10 paid plan | pass | The bind is `pendingPayment` with zero periods. The quote was 400 USD minor for the 4 cr fee. The signed payment moved the plan to `active` with period 0 authorized by `paymentOrder`. A call consumed 61 of the 100 quota. The next three calls were refused 402, and the period's `consumedMinor` stayed 61, so the quota was not overdrawn. Overage is disabled and the wallet had already been refunded. Cancel gives `cancelAtPeriodEnd: true` and the plan stays `active`. |
| J11 statement | pass (paid path) | Plan statements were issued and `paid` (total 4,000,000), visible in both `/api/v1/org/statements` and `/business/statements`. The customer Statements page now points to "Plan invoices live under Billing" (#1073). The open-receivable and dunning path was not exercised because no credit facility exists. |
| J12 reports | pass | Financial facts: usage 244, rated 244, entitlement 61, funding 30,000,000, billed 8,000,000, collected 8,000,000, deferred 8,000,000, refund 20,000,000, receivable 0. These equal the recorded actions above. The daily reconciliation run for 2026-11-14 is `matched` with complete coverage. The live day is not closed. |
| J13 offline verification | pass | The candidate's own `verify-oxsum-bill-0.1.0.tgz` (sha256 `4720ef72…`) was installed in a clean consumer. Receipt, statement evidence and the nine negatives all passed offline. |
| J14 deploy / rollback | pass (local compose) | `oxsum-accept` started on `cc16bb8f` (0131) and was seeded. Steps: upgrade to the candidate (migrate 0131 to 0139, ready, `gitSha 8dface36…`); attempt to return to `cc16bb8f` (`/readyz` never ready on the 0139 DB, so the schema-mismatch refusal held); same-schema rollback to `8cca80a2` (ready); re-deploy of the candidate (ready). Org, user and business persisted, with 133 migrations. This is not a GHCR `deployment_plan` and not a live deployment. |
| J15 capacity + 8h soak | **blocked (external)** | Needs the declared 4-vCPU/8-GiB Linux reference host (#742). Not run. |
| J16 upgrade / backup / restore | pass | (a) `rehearse-artifact-recovery.py` with old `cc16bb8f` (0131), current `8dface36` (0139) and rollback `8cca80a2` (0139) native binaries, all extracted from their candidate images. Drain took 281 ms. The upgrade migration took 114 ms with credential and signing key retained. Backup/restore and compatible rollback passed, and old and restored proofs verified offline (0 network fetches, tampered inputs rejected). (b) `backup_restore.py` on the populated `oxsum-fresh` DB went to a separate PostgreSQL target: receipt OK (schema 0139, organizations 4, usageRecords 9, 1234 ms). The candidate served the restored DB ready, and owner login returned 200. A second restore into the existing target was refused. |
| J17 two pilots | **blocked (external)** | Two independent outside participants are needed (#745). No external contact was made. |
| J18 embedded application | pass (isolated rehearsal) | `examples/embedded-application/app.py` drove the candidate over HTTP with a ten-scope application credential: signup, key, pending checkout refused for paid use (402), webhook fulfilment, ask (61 minor), trust, receipt, plan checkout, paid statement, publish, evidence, then archive (a later ask was refused). Bob's lost response replayed with `idempotent-replayed: true` for the same answer and exactly one usage row (61). Changed inputs were refused. A narrow-scope credential got 403, a foreign application got 404, and a revoked credential got 401. |

Other runs on the exact SHA: the `embedded_application` integration test is in
full CI, and the developer-examples fresh install is the candidate `examples`
gate.

## Operator notes (not defects)

- The first `backup_restore.py restore` call omitted `--database` and was
  refused. It succeeded with `--database postgres`, and nothing was dropped.
- On a 0139 database, the older `cc16bb8f` binary's `--migrate-only` exits 0
  and prints `schema migrated to 0131…`. `/readyz` stays not ready, as
  documented in `docs/self-hosted-upgrade.md`. The container still answered
  `/api/v1/version` while not ready. `deploy-candidate.py` gates on readiness
  and version.

## Remaining for the N18 verdict

- J15 needs a declared-hardware capacity/fairness run and an eight-hour soak (#742).
- J17 needs two independent pilots (#745).
- Real-provider (#740) and PSP (#741) tiers stay separate. Publication and
  deployment (#746) need their own authorisation.
- Raw logs, screenshots and app stores are retained off-repo on the operator
  host under `/tmp/oxsum-fresh-8dface36/evidence`, `/tmp/oxsum-accept-8dface36/evidence`
  and `/tmp/oxsum-j16/run1`. They contain fixture secrets and session content
  and are not committed.
