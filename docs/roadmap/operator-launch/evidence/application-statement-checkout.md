# Scoped application statement debt checkout

Issue #935, `feat/application-statement-checkout`. Joins #929 (`0546c33a`)
and #933 (`de4149ae`) in prerequisite merge `8ab78c60`; no new migration.
Application evidence publication #934 is independent of this payment extension.

The existing checkoutWrite grant can select exact named statement debt, with
an optional credit-minor partial amount. Original source allocations, collection
revision and merchant route/mode are server-quoted. Provider fiat ceilings do
not alter debt settled. Pending portal/foreign application order IDs are omitted.

Application authority and exact replay are checked inside the existing order
transaction; original debt ownership/append locks recheck the source fingerprint.
The reservation, order, merchant binding and application association commit
atomically. Unknown external outcomes remain reserved and recover the same order
and destinations. Fulfillment/refund/loss/source-void use existing statement
payment code, without wallet funding or entitlement activation/renewal.

Executed development checks:

- Ten pre-existing joined application core/HTTP cases passed (2.235s).
- Eleven core cases including exact partial/full debt and concurrent reservation
  passed (2.450s); twelve final core cases including mixed historical/daily-source
  refund and loss passed (2.706s).
- The 32-case affected core/HTTP selection passed 31 cases. One HTTP fixture
  supplied a generic provider ID instead of the real local checkout's retained
  ID; production correctly refused to fulfill it. The fixture now confirms the
  retained ID. Its final HTTP unknown-outcome/recovery/verified-payment sequence
  passed (1.972s), with exact debt, zero wallet credit and unchanged periods.
- Four OpenAPI checks, generated TypeScript and production frontend build passed.
  Existing chunk-size warning remains.
- Joined-stack complete workspace passed all 2,205 tests (349.782s), with six
  existing opt-ins skipped. Strict core/server all-target Clippy and pinned
  formatting passed. The final 13-case core run passed (3.902s), including one
  added case proving changed collection terms refuse before reservation and
  portal order IDs remain invisible while their debt remains reserved.
  All 2,206 ordinary cases have passing full/focused evidence; unchanged full
  passes are reused. No production source changed after the complete run.

No paid provider or real PSP call occurred. Independent review, protected
integration, browser, real PSP and exact candidate J18 acceptance remain pending.
Signed lifecycle notifications and the separate Python/SQLite application remain.
