# Operator-launch browser acceptance — candidate runtime

Date: 2026-11-12. Journeys J02→J12, candidate-acceptance tier. Parent
operator-launch acceptance; development-only, not an external certification.

## Runtime identity

- Image `ghcr.io/allurecurtain/oxsum:sha-cc16bb8fe5d6875ee2bb054e2c436cc788fc94ef`.
- Git SHA `cc16bb8f` (frozen candidate), binary version `v0.1.0`.
- Schema `0131_invoice_corrections`; `GET /api/v1/health` reported
  `ready (0131_invoice_corrections)`.
- Compose project `oxsum-accept` on `http://127.0.0.1:3210`; dedicated
  PostgreSQL database `oxsum_accept`. Owner `owner@example.test`.
- Mail capture: local Mailpit `http://127.0.0.1:8225` only; invites used
  `.test` recipients. No external inbox.
- Provider supply: task-owned loopback OpenAI stub on `192.168.65.254:3211`
  (`accept-chat`), and a task-owned Hyperswitch-protocol PSP stub on
  `192.168.65.254:3212`. **Neither is a real provider or PSP.** The merchant
  SDK URL is an unserved `https://sdk.invalid/` placeholder; secure
  checkout/SDK execution is untested.
- Payment events below are explicit local injections signed with fixture
  secret `whsec_fixture_signing_0001`, HMAC-SHA512 over exact bytes, header
  `x-webhook-signature-512`. Each returned HTTP 200 and landed
  `state=applied` in `oxsum.merchant_event_inbox`. No bank or PSP money moved.
- Environment tier throughout: **candidate-image / local-fixture**. No real
  PSP, provider, HTTPS SDK, or external delivery was exercised.

## Actor roles used

- **Owner** (`owner@example.test`): business console, all sections.
- **Customer A** (`cust-a@example.test`, org `d4da7ae1-…`, key `oxs-45a2…`):
  customer portal and API key calls.
- **Finance** (`finance@example.test`) and **Support** (`support@example.test`):
  invited staff roles.
- All pages shared one cookie session; each role switch re-logged in.

## Observed journeys

| Path | Actor | Observed outcome | Evidence |
| --- | --- | --- | --- |
| J02 — owner setup, business profile, readiness | owner | Business profile/setup completed; console sections reachable; readiness checklist inspected. | `j02-01-setup-done.png`, `j02-02-business-console.png` |
| J03 — channel connection, secret rotation, denied authority | owner | Loopback channel approved; secret rotated AAAA→BBBB; J08 stub log confirms the new bearer `fixture-loopback-key-BBBB2222` on the served call. | `j03-01-channel-detail.png`, `j03-02-rotation-done.png`, `model_stub_requests.jsonl` |
| J04 — sale book, customer terms, price preview | owner | Draft published, customer A book assigned, override scheduled; customer preview resolves exact book/version/schedule. | `j04-01-price-preview.png`, `j04-02-customerA-preview.png` |
| J06 — customer invitation, registration, invite-only policy | owner + customer A | Invitation mailed via local SMTP; customer accepted the reserved invite, key shown once; open registration toggled and restored to invite-only. | `j06-01-customer-overview.png`, `j06-02-invite-only-restored.png` |
| J07 — staff invitation, roles, revocation | owner + finance + support | Finance/Support invited under role emails; role-scoped consoles verified. **Defect below.** | `j07-01-finance-role.png`, `j07-02-support-role.png`, `j07-03-revoked-support-loginloop.png` |
| J08 — readiness, first bill, settlement, evidence | customer A | SSE call settled against loopback; transaction log links each entry to `/verify?entry=`; statement evidence bundle fetched (`/api/v1/org/statements/{id}/evidence`, 8965 bytes) and saved. | `j08-01-chat-settled.png`, `j08-02-verify-entry.png`, `j08-03-verify-result.png`, `j11-statement-evidence.json` |
| J09 — order → payment → refund recovery | owner + customer A | Top-up order `36d6af06` provider-pending then fulfilled on signed `payment_succeeded` (event `evt_accept_pay_0001`, applied). Refund `0c3f72bd` approved; signed `refund_succeeded` (event `evt_accept_refund_0001`, applied) produced a `succeeded` refund and applied credit note `094f2624` of 19,999,920 minor; wallet returned, zero suspense. | `j09-01-order-fulfilled.png`, `j09-02-refund-succeeded.png`, `merchant_event_inbox`, `oxsum.refunds`, `oxsum.credit_notes` |
| J10 — paid plan lifecycle | owner + customer A | See section below. | `j10-01..j10-06` |
| J11 — postpaid statement / receivable | owner + customer A | See section below. | `j11-01..j11-05` |
| J12 — financial explanation / reports | owner + customer A | See section below. | `j12-01-biz-reports.png`, `j12-02-usage.png` |

## J10 — paid plan lifecycle

Owner bound Customer A to sold plan version `7989275a-…` (Acceptance paid plan
v1: exact **4 cr** fee, **100-minor** quota, anniversary month, overage
enabled, recognition policy v2). The bind opened `pendingPayment` — `GET
/api/v1/org/plan` returned `state=pendingPayment`, `activatedAt=null`, **zero
periods** — a timer alone granted nothing (forbidden-effect check).

Customer A reviewed **1.01 USD** (0.250001 USD/credit rounds the fiat up while
the sold 4 cr fee stays exact) and opened order
`545bd110-4114-4ee9-9d3d-bb2920d0aa4c` (purpose `planPeriod`, 101 USD minor).
It stayed `provider pending` until signed event `evt_accept_pay_0002`
(`amountMinor 101`, `paymentId=order id`) applied; the order then fulfilled and
activated period 0 (`2026-11-12T04:25:17Z → 2026-12-12T00:00:00Z`), authorized
by `paymentOrder` naming the same order id. Wallet credit did **not** increase.

Quota consumption: a temporary paid override (1.5/2.5 cr per million) made a
`max_tokens:10` call reserve 66 minor and settle 22 minor — `consumed` reached
**22**, then **44** after a second call. A third call needing a 66-minor
reservation while only 56 remained was refused `402 INSUFFICIENT_FUNDS` —
remaining quota honored, never overdrawn. The free-rate override was then
restored. The customer cancelled through the confirmation dialog: plan stayed
`active` with `cancelAtPeriodEnd=true`, the paid period still serving — no
mid-period pro-rating, no renewal. Early-renewal was correctly refused (quote
answered "manual renewal becomes available at the current period boundary", no
pay control while the period is open).

## J11 — postpaid statement / receivable

The period-0 close issued plan statement `15d28ed8-fc74-4703-9e72-b9ad63598d20`
(journal `b1f6feeb-…`): `planFee +4,000,000` minor reconciled by
`paymentApplied −4,000,000`, total/paid 4 cr, owed 0, `state=paid`, due
2026-11-26. The repayment surface correctly reported "no payable issued debt"
(outstanding/pending/available all 0). The customer `/app/billing` table and
business Statements tab both list it; invoice journal row is `paid`/settled
4 cr. Business Receivables aging buckets (current, 1–30, 31–60, 60+) all read
0 cr / 0 open — the only receivable is fully settled. The wallet-monthly
`/api/v1/statements` is empty (no wallet-credit period finalized; customer's
credit limit is 0 so no postpaid draw exists). Honest boundary: an *open*
receivable / past-due dunning / statement repayment flow could not be exercised
without a draw facility — only the paid-statement path is evidenced.

## J12 — financial explanation / reports

Business Reports financial facts reconcile to recorded facts: **Usage**
0.000044 cr (two 22-minor paid calls), **Rated** 0.000064, **Entitlement**
0.000044 (plan-quota draws), **Funding** 23.99992 cr (20 cr J08 top-up + 4 cr
plan order), **Net billed** 4 cr (plan fee), **Collected** 4 cr, **Refunded**
19.99992 cr (J09 take-back), **Receivable** 0, **Recognized income** 0 /
**Deferred movement** 4 cr (period open, daily accrual not yet closed). Cost
coverage recorded 11 untracked turns (no supplier-cost terms). One daily
reconciliation run for 2026-11-11 finished `matched`, **0 exceptions**.
Customer Usage page: the `accept-loopback` channel shows 4 turns
(27 in / 9 out under `default` + 9 in / 3 out under `chat`) = the 44 µcr
charged to the plan; the 7 pre-rotation calls show 0 charged.

## Defects

- **Revoked-staff dead-end loop (J07).** A revoked user whose session resolves
  to `organizationId:null, business:null` cannot reach any page: `/` routes to
  the customer shell, `/session` returns 401, the app navigates `/login`; after
  a fresh login the app navigates `/` and bounces back — a dead-end loop.
  `POST /api/v1/auth/login` for the revoked identity returns
  `{"business":null,"organizationId":null}` 200, so there is no landing target.
  Evidence `j07-03-revoked-support-loginloop.png`. Source:
  `web/src/components/AppShell.tsx` + `web/src/pages/auth/Login.tsx`.
- **Stale customer Statements page.** `/app/statements` reads
  `/api/v1/statements` (wallet monthly), which is empty, while the paid plan
  receivable lives under `/app/billing` reading `/api/v1/org/statements`. The
  two surfaces show different statement sets; the paid plan statement is not
  visible on the customer Statements nav page. Not a data error, but a
  discoverability/consistency gap worth a note.

Filed as [#1072](https://github.com/AllureCurtain/oxsum/issues/1072) (revoked
dead-end loop) and [#1073](https://github.com/AllureCurtain/oxsum/issues/1073)
(statement surfaces) — recorded here, repaired separately, not claimed passed.

## Remaining acceptance not claimed here

- Real PSP / provider / HTTPS SDK certification; all payment paths used the
  local stub and explicit signed injections.
- An *open* (unpaid/past-due) receivable, statement repayment, dunning and
  credit-draw path — no draw facility existed on the customer.
- Period-end rollover, actual renewal, expiry and daily accrual close — no
  period was artificially closed or backdated.
- J05/J09 full PSP recovery depth, J13–J18 (protected integration, deploy,
  restore, embedded app, pilots), reference 8-hour soak, external inbox and
  pilot parties all remain separate gates.

## Artifacts retained off-repo

Sanitized screenshots (`j02-*` … `j12-*`), the loopback upstream request log
(`model_stub_requests.jsonl`), the Mailpit capture and the compose environment
live under the ignored `/tmp/oxsum-accept/` task directory; they contain session
content and are not committed, matching the convention in
[browser-acceptance](../browser-acceptance/README.md). The statement evidence
bundle is committed here as [j11-statement-evidence.json](j11-statement-evidence.json)
(8,965 bytes, fixture identities only).
