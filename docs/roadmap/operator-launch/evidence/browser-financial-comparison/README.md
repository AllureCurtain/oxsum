# Browser financial comparison

Date: 2026-11-10. Finance used the portal to request a booking-date comparison
covering November 10–11 (inclusive) after bundle return, paid-plan chargeback,
1+3-credit statement repayment, source-specific journal void and both repayment
returns. Local model/payment fixtures only. Run `14e30bbb-a059-442f-80f1-f773f30ca809`,
retained snapshot `2425c5f1-aa5e-4603-ab22-37380fc095d4`.

All **15/15 sources** and **20/20 entries** across three customer organizations
were verified. Expected and actual are identical for the six compared fields:
billed/earned/deferred/collected/receivable are zero; confirmed refund Cash is
**8,000,000** minor (4,000,000 bundle plus 1,000,000 and 3,000,000 repayment returns).
The captured evidence preserves source IDs, canonical entries and differences.

The run correctly remains **exceptions**, with four `missing_cost` entries for
settled local-fixture calls. These were not waived or filled with invented cost.
The five-turn cost-coverage display includes another tracked request category;
it is not the same denominator as settled missing-cost exceptions. Provider
cost, usage, funding and actual service delivery are explicitly outside this
six-field comparison. A matching comparison is not complete business accounting.

Date controls were entered with browser keyboard segments: the automation's
`fill` updated the native DOM but did not update React state, producing 400 for
empty dates. That tool limitation was not treated as an application defect.
The successful request preserves literal YYYY-MM-DD inputs. Its response exposed
a separate real contract defect: period dates were year/ordinal arrays, displayed
as concatenated digits. #983 repairs serialization; this baseline does not claim
that repair passed. No accounting data was modified for the date diagnosis.

Backend source `ac7bb191ee7a2b52af4a23f2ce54cd9c6f9d82e5`, SHA-256
`941e9f1776e64ed5559074c637bf73d67de7380a382dc12f56e1b149ca610c53`; frontend
source `b51c5262` (paid-journal correction). Screenshots were visually inspected.
This is development source/ledger evidence, not a release, PSP or pilot gate.
