# Paid journal correction and original-source returns (#981)

Date: 2026-11-10. Development browser; local signed payment protocol fixture only.

## Portal repair

The paid journal in [dispute repayment acceptance](../dispute-outcomes/README.md)
had no Void action despite existing core/API support. The repaired list exposes
the same FundsWrite operation for posted and paid journals. Its keyed component
retains a fixed journal/reason/idempotency request after ambiguous errors, including
terminal-state refresh and cancel/reopen; only confirmed success clears it.

Owner used the actual confirmation on paid journal
`014850df-4712-45f0-833a-a193d1808eed`. A page-local transport fault deliberately
lost the successful response. Error and disabled reason survived cancel/reopen,
and the byte-identical replay returned 200 and closed the dialog. Exactly one
`journal.void` audit row and one canonical reversal entry exist.

Entry `cef59e9e-784a-5b93-84ad-46b9dfc5c49b` debits UnearnedPlanFees **4,000,000**
and credits RefundPayable **4,000,000** minor. There are **no Cash postings**.
The immutable original journal header keeps its gross amount; the +4,000,000
original fee and -4,000,000 reversal net to zero. Both source-owned statements
become void with zero total/owed, preserving original lines and reverse sources.
Read-only database evidence records payable shares of 1,000,000 and 3,000,000
against the exact two original repayment effect IDs and the same reversal hash.
Wallet available remains **999,956**, held zero after the void.

## Customer request and Finance return

Customer opened each original repayment order and requested its exact remaining
1-credit and 3-credit return. Finance approved both through the portal. Refunds
`7433a600-35ab-4bb0-b767-28eab84b3d1a` and
`00ed024a-c0d8-43f9-a4cc-f59283c92df0` became provider-pending first, then succeeded
only after explicit local signed callbacks. Each bad signature returned 401 and
the valid callback/exact replay returned 200. Both credit notes are applied.

Verified effect responses retain the original payment effect/hash. Returns use
Cash/payable **1,000,000** and **3,000,000**, with **zero receivable and suspense**;
no debt is reopened. This confirms source ownership after the void. The unrelated
historical manual-receipt pending row remains outside successful return claims.
The fixture SDK remains unserved; this is not real PSP settlement certification.

## Validation and artifact

Frontend source `b51c5262`, byte inventory attached. Backend source
`ac7bb191ee7a2b52af4a23f2ce54cd9c6f9d82e5`, SHA-256
`941e9f1776e64ed5559074c637bf73d67de7380a382dc12f56e1b149ca610c53`, schema
`0128_application_notifications`. No financial backend code changed.
Five contract checks, TypeScript/Vite build and pinned formatting passed.
Actual browser success, ambiguous retry and separate customer/Finance return
checks passed. Screenshots were visually inspected; public fixture evidence and
read-only accounting extracts contain no credentials or payment client secrets.
Independent review/integration and final candidate acceptance remain; no complete
monthly postpaid, boundary/renewal, PSP, capacity or pilot claim is made here.
