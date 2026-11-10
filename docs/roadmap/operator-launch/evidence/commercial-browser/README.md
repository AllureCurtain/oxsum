# Commercial browser follow-through (#969)

Date: 2026-11-10. Parent #946; development acceptance only.

## Environment and limits

Owner, customer and Finance used the real React portal on loopback in the same
isolated database as [initial browser acceptance](../browser-acceptance/README.md).
Backend source `ac7bb191ee7a2b52af4a23f2ce54cd9c6f9d82e5`, binary SHA-256
`941e9f1776e64ed5559074c637bf73d67de7380a382dc12f56e1b149ca610c53`.
Frontend source `879e6e6f` plus manual-refund repair `b87b4816`; byte inventory
and artifact checksums are attached.

The owner created/enabled/selected merchant `0b79a317-c191-40d8-8962-440bc452998d`,
version `35a976c5-ca3b-4226-af00-7c97edb3573f`, route revision 1 through the UI.
Its API is a task-owned loopback Python protocol stub, **not** a real Hyperswitch
stack or PSP. The SDK address is an unserved `.invalid` HTTPS placeholder;
secure checkout/SDK execution is untested. Signed success events below were
explicit local injections. Each bad signature returned 401; the original valid
event and its exact replay returned 200. No bank or PSP money moved.

## Bundle purchase and return

The owner published `Browser bundle fixture`, version
`006da06d-79c8-4bd2-b247-4f5fbd4fda4e`: USD 1 purchases 4 credits plus a separate
0.5-credit promotion, expiring seven days after payment. The customer explicitly
selected the bundle and local merchant. Order
`25b56854-8af8-4921-ba5a-b2da05ac16fb` remained provider-pending before the signed
fixture event, then fulfilled once. Wallet available changed **999,956 →
5,499,956** minor, with zero holds.

The customer requested the full return; Finance approved refund
`ab3f4d29-d587-40a6-8c77-067026314239`. The stub received one refund POST. The signed
local success/replay produced one succeeded refund and one applied credit note:
**4,000,000** minor Cash and original Wallet return, zero suspense, with the
**500,000** unused promo cancelled. The wallet returned to **999,956**, holds zero.
Customer and Finance screenshots show the confirmed effect. The original order
stays fulfilled and historical approval is retained; the refundable remainder is
zero. This is local accounting/authority evidence, not external payout evidence.

The unrelated historical manual receipt refund still visible in the Finance
screenshot is the defect documented in [#966](../manual-refund-route/README.md).
It has no cash effect and is not counted as a successful return.

## Paid plan, statement and cancellation

The owner published and bound `Browser paid anniversary`, version
`d950fd07-2062-4dbe-8605-6fc834b05e24`: exact **4-credit fee**, **100-minor quota**,
anniversary period, overage enabled, daily recognition policy v2. The customer
reviewed **1.01 USD** at the existing 0.250001 USD/credit conversion. This correctly
rounds the collection amount upward while preserving the sold fee.

Order `c520b6ff-fb9c-497a-87d8-30dbf6758818` remained pending before its local signed
payment event and activated one period afterward, from
`2026-11-10T05:46:44.25686Z` to `2026-12-10T00:00:00Z`. Wallet credit did not increase.
The Billing page showed statement `ea5a4f62-62e6-4804-94bf-ae09043ca450`: fee
**4,000,000**, payment **−4,000,000**, paid **4,000,000**, owed **0** minor. Source
references name the exact period and payment order. Repayment was unavailable
because there was no outstanding debt. Early renewal returned 409 and the page
explained that manual renewal begins at the current boundary.

A customer Chat call used the local SSE fixture: 9 input / 3 output tokens,
40-minor quota consumption, wallet unchanged. The customer cancelled through
the confirmation dialog. The plan remained active with cancellation scheduled
at period end; another real browser call consumed a further 40 minor. The quota
engine read **80/100** consumed; wallet remained **999,956**, holds zero. No
period was backdated or artificially closed, so daily accrual/actual month-end
and renewal-boundary behavior are not claimed by this browser run.

## Defect found and remaining acceptance

The Plans page and `/org/plan` returned zero consumed even though the exact
subscription counter read 40 and then 80. The baseline screenshot and paired
JSON retain this discrepancy. [#973](https://github.com/AllureCurtain/oxsum/issues/973)
repairs the period read; this evidence does not claim the display repair passed.
The screenshots were visually inspected and JSON was checked for secrets before
submission. None contains payment client secrets, passwords, API credentials or
webhook signatures. Only fictional local identities and public fixture IDs appear.

Statement debt repayment/correction/dispute, embedded browser consent and
notification setup, genuine PSP/HTTPS/SDK, exact candidate, reference capacity,
restore and independent pilots remain separately required. Local success above
does not close N10/N11/N15/N18 or the complete J09–J12/J18 journeys.
