# Dispute outcomes and customer repayment (#978)

Date: 2026-11-10. Development browser acceptance in the same isolated database
as [commercial acceptance](../commercial-browser/README.md). Local signed payment
protocol fixture only; the HTTPS SDK URL is unserved and no real PSP money moves.

## Explicit decision

The old portal accepted only a reason and always sent `closed`. The repaired
English/Chinese dialog requires one of evidenceDue/won/lost/closed and describes
its financial effect before confirming. The existing refundsWrite authority and
API contract are unchanged. Empty outcomes/reasons cannot submit.

Owner used the browser to move local case `local_browser_plan_dispute` through
`evidence_due` and `lost`. The second operation had a fresh decision identity.
Original paid order `c520b6ff-fb9c-497a-87d8-30dbf6758818` created one 4,000,000-minor
Cash loss / receivable effect, zero payable and review suspense, under entry
`b5821ee1-7764-5ca3-b2fc-b800b316a548`. New debt statement
`dd231630-25ad-4724-85c8-01cff23ff3c5` retains the original journal and dispute
source. The original paid statement remains unchanged. The journal's legacy
`disputedMinor` counter is cumulative at intake and recharge (8,000,000 here),
not a second Cash loss or the outstanding debt; that legacy metric is unchanged.

## Partial and complete repayment

The customer opened Billing, reviewed **1 credit / 0.26 USD**, selected the local
test merchant and created order `fddb565d-21e3-4681-859c-6e8caa5352f8`. Pending did
not settle debt. The explicit signed fixture callback reduced outstanding debt
from 4 to 3 credits. A second browser checkout for **3 credits / 0.76 USD**, order
`1a5e27bc-1013-4035-bd97-315f6d7a92b2`, settled the remainder. Upward fiat rounding
is per payment; exact credit debt is unchanged. Each callback was checked with
bad signature 401, valid 200 and exact replay 200.

The final document is paid, total/paid 4,000,000 and owed zero, with exactly two
source-linked payment allocations (-1,000,000 and -3,000,000). The wallet stays
999,956 minor with zero holds. No wallet credit or renewal is created. This is
plan-chargeback debt repayment, not the separate monthly postpaid journey.

## Ambiguous response and refresh

A page-local fetch wrapper let the actual `won` request commit, then deliberately
threw away its response. The first version exposed a real UI bug: its 30-second
list refresh unmounted the pending decision on a terminal row. The repair keeps
the component keyed by dispute and mounted while an unconfirmed intent exists.

A fresh local case `local_browser_retry_preserved` repeated the fault. The dialog
and error survived the actual refresh to `won`; outcome/reason stayed disabled.
Cancel/reopen retained the request. Retry sent **byte-identical JSON**, including
idempotency key, and the successful replay closed the dialog. `retry-attempts.json`
retains both requests. Both retry fixtures ended won and created no money effect.
No credential, signature or payment client secret is in this evidence.

## Artifact and validation

Final frontend source `2537a551` (byte inventory attached). Loss and retry backend
source `ac7bb191ee7a2b52af4a23f2ce54cd9c6f9d82e5`, SHA-256
`941e9f1776e64ed5559074c637bf73d67de7380a382dc12f56e1b149ca610c53`.
Repayment browser used source `3b5b55be4b54cf4bf9ba35c87d6119bd4b108ff7`, SHA-256
`4f56cc7735524163b08fe2718ad03450609569c389c978e5dadd07a88bc4bef8`, with unchanged
repayment code. Both use schema `0128_application_notifications`.

Five contract checks, TypeScript/Vite build and pinned formatting passed. All
screenshots were visually inspected. Frontend-only change; existing financial
regression evidence is retained, not rerun as a substitute for browser checks.
Independent review/integration and final exact-candidate acceptance remain;
this evidence does not close all J09-J12, real PSP, postpaid, period-boundary,
capacity, restore or pilot gates.
