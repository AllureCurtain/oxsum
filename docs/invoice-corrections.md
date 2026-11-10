# Source-linked invoice correction and reissue

Implementation specification for [#1027](https://github.com/AllureCurtain/oxsum/issues/1027).
This workflow is under implementation; the existing journal void is not a reissue.

## Commercial meaning

A correction replaces one proven invoice for the same sold service. It does not
authorize a new service interval, quota, plan version, currency, tax or usage
charge. The operator selects original charge lines and a lower or equal exact
integer charge for each. Existing discounts and promotional credits remain at
their proven amounts. Unselected charge lines remain unchanged. The resulting
net fee must be nonnegative and no greater than the original net consideration.
The reason explains the commercial reduction; a balanced ledger alone cannot
justify a new charge. Unsupported historical usage must first complete its
existing source-linked repair; tax has no current originating writer.

A live original is financially voided and its successor is issued in one
transaction. An already voided original may be reissued only from its retained,
verified pre-void lines and financial sources. Void reversal lines are not
charge inputs. Each original can have at most one successor. A correction chain
uses the immediate predecessor's surviving consideration as its ceiling, so a
later correction cannot restore an earlier discount. All original documents,
line identities, payments, refunds and ledger entries remain retained.

## Payments and returns

Reissue does not transfer original cash or silently apply an old refund payable.
The original's still-collected amount becomes refund payable through its exact
financial void. Previous cash refunds or chargebacks have already reduced that
retained amount. Existing repayment allocations retain their individual payable
ownership. For an already voided source, the remaining payable must be derived
from the verified void and subsequent source-specific returns, not from its
stale `settled_minor` header.

The successor starts unpaid for its corrected net consideration (paid when
zero). Its outstanding amount and the original-source payable appear separately
in the preview and confirmation. For example, a 100-minor paid invoice corrected
to 80 produces 100 minor owed back on the old source and an 80-minor successor
receivable; the operation moves no Cash. A 100-minor invoice with 30 collected
produces 30 payable and an 80 receivable. Finance must use the existing explicit
original-source return and successor payment workflows. The UI must not describe
this as a net charge, a refund execution or an automatic credit transfer.

Original returns continue against their original void/payable allocations.
Successor payments use named-statement repayment and its existing payment,
reservation, return and dispute identities. A successor return reopens only its
own proven receivable; excess authenticated cash loss stays explicit review
suspense. No mutation may infer payment ownership from a current account balance.

## Accounting generations

Policy 1 reverses the predecessor's remaining net earned fee through its existing
void and posts the successor's corrected fee as an accrual. Policy 2 first catches
up completed UTC service days, then reverses the predecessor's earned/deferred
position. Its successor uses the original exact service interval, with earned
consideration equal to the integer cumulative share at the completed boundary;
the balance remains deferred. Its counter-entry is Receivables, never Cash.

Retain the one original `plan_accounting_periods` row. Add an immutable successor
accounting context referencing that original snapshot and the immediate
predecessor, together with correction/source-line records. Daily effects on a
successor carry its journal identity. Historical effects without an explicit
journal still belong to the original accounting row. Recognition uniqueness,
effect totals and the worker cursor must distinguish journal generations. Every
generation recognizes each eligible UTC day once, including a zero-amount day.
Reissue records its exact completed recognition boundary so the independent
comparison can order zero-value effects without guessing from wall-clock order.
Closing a service period must continue finding the original authorization and
must never issue an extra fee because an earlier invoice was corrected.

Both generations conserve `billed = earned + deferred` and
`receivable = billed - collected`. Source comparison independently reconstructs
the correction cap, void, replacement entry, retained payment ownership and
recognition schedule from immutable sources and official ledger hashes. Header
agreement is insufficient. Financial reports include replacement movements once;
customer evidence retains the correction, original line references, accounting
context and entry hashes while preserving already frozen export bytes.

## Authority, preview and commit

Both preview and apply require a current personal Business `FundsWrite` session.
Preview captures a bounded repeatable-read snapshot and makes no financial,
document or provider mutation. Its source revision commits the exact customer
financial sources, official hashes, selected correction lines, immediate
predecessor and completed recognition boundary. Preview returns original terms,
line-level reductions, successor earned/deferred/receivable and original-source
payable separately. It does not reserve or promise a future result.

Apply binds the actor, target, reason, exact canonical line selection, source
revision and idempotency key. Lock Business, subscription/all customer plans,
journal, ledger append and affected statements in the existing order. Validate
source comparison and current revision again before any effects. Live payment
intents on affected documents refuse. Existing completed returns are included;
in-flight returns must be resolved before correction. A concurrent close,
recognition, repayment, refund or dispute either precedes capture or makes the
preview stale. Source inconsistency refuses without guessing or partial effects.

Void, replacement journal/statement, line references, accounting effects,
correction receipt and actor audit commit together. Replay checks the retained
request before recomputing current sources: an exact retry returns the original
receipt and IDs, even after later payments or another correction. Altered replay,
another successor or stale revision conflicts. Customer and staff detail pages
navigate both directions and retain the exact submitted request after an unknown
response; dismissing a dialog does not generate a replacement payment intent.

## Acceptance still required

- Real-ledger unpaid, partially paid, paid, refunded and already-voided sources;
  both policies, multiple original payment sources and correction chains.
- Source-line bounds, unchanged credits, zero and maximum integers, completed
  service boundaries, no new entitlement and no repeated recognition.
- Current authority, foreign source rejection, immutable history, exact replay,
  stale/concurrent requests and injected failure atomicity.
- Successor debt/dunning, payment and returns; original payable refunds/disputes;
  independent source comparison, reports and offline evidence.
- Core/HTTP focused checks, one complete cross-crate run, strict changed-crate
  Clippy, contract/codegen/frontend and pinned formatting.
- Real Finance/customer browser confirmation, lost-response recovery, linked
  documents and successor payment against isolated signed fixtures. Genuine PSP
  and final-candidate certification remain their own execution gates.
