# Invoice correction browser acceptance

Issue [#1027](https://github.com/AllureCurtain/oxsum/issues/1027). Observed
2026-11-11 with Ego Lite against a locally built `oxsum` binary and a dedicated
PostgreSQL database (`oxsum_ic_accept`). The fixture was produced through the
production path — a published sold plan bound to a registered customer, paid
through a test-mode merchant's signed event and fulfilled — so the corrected
journal and statement are real ledger documents, not seeded rows. No real PSP
or provider call was made; the merchant webhook was a locally signed fixture.

## Observed journey

| Step | Observed outcome |
| --- | --- |
| Owner fixture | Business initialized via setup token; registration opened; priced `calendarMonth` plan (2.5 cr, `accountingPolicyVersion` 2) published and bound to a new customer; paid order activated the period and issued journal `f7642530` + statement `82735665`, both `paid`. |
| Preview | In Business → Statements → Invoice journals, "Correct invoice" opened the dialog listing the original base-fee line at 2.500000. Editing it to 2.000000 and previewing returned: same service period retained, original funds owed back 2.5 cr, new invoice to collect 2 cr, recognized 0 / deferred 2 cr, and the explicit "No cash moves now" notice. |
| Confirm | With a reason entered, "Cancel original and issue new invoice" committed and reported "The original invoice was cancelled and its successor was issued," with links to the new invoice `6a60a3cf` and new statement `e438940e`. |
| Chain | The original `f7642530` became `void` with `successorJournalId` set; the successor `6a60a3cf` was `posted` at 2 cr with `replacesJournalId` naming the original. A second correction on the successor and a third on its successor produced `5cdd46a7` then `6b77a431`, a continuous three-generation chain, each preserving the original service interval and due date. |
| Recovery/idempotency | Replaying the exact committed request over HTTP returned the byte-identical receipt (same `correctionId`, `journalId`, `statementId`, preview). A stale `sourceRevision` returned 409 "invoice correction preview is stale"; reusing the idempotency key under changed terms returned 409 "key names different request terms". |
| Customer view | In the customer portal's Billing, the outstanding plan debt read 1.2 cr and Statements listed the live issued statement `833012a8` (1.2 cr) above the three voided predecessors. A voided statement's detail showed "Open replacement document" navigating forward to its successor, and "Download and verify statement evidence" remained available. |
| UI evidence | `journal-chain-en.png` — journal table with the void/predecessor chain and live successor; `journal-detail-en.png` — corrected journal list; `journal-chain-mobile-en.png` — 390×844 rendering; `customer-voided-successor-en.png` — customer statement detail with the forward replacement link. Prior session captures `finance-successor-zh.png` and `recovery-mobile-en.png` remain. |

## Remaining acceptance

Genuine PSP correction, a final-candidate certification on the exact release
artifact, and independent review remain their own gates. This record covers the
development browser journey only.
