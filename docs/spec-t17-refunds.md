# T17 — Corrections, partial refunds and disputes (issue #608)

Design decisions for the implementation. Follows T15's shape: durable rows
before any PSP call, ledger keys derived from durable ids, signed webhooks the
only source of terminal PSP truth.

## Schema (migration 0091)

### `oxsum.credit_notes` — EXTEND existing table
Existing table (issue #352): `(credit_note_id, organization_id, entry_id,
amount_minor, reason, created_at)`. T17 needs it to name the order it
corrects. Add nullable columns rather than a second table:

- `order_id uuid REFERENCES oxsum.payment_orders(order_id)` — the funding
  event the note corrects (NULL for the legacy admin free-credit notes).
- `refund_id uuid` — the refund the note was issued for (NULL until applied).
- `state text NOT NULL DEFAULT 'applied'` — `draft | issued | applied |
  reversed`. Legacy rows are `applied` (the ledger entry already stands).
- `issued_by uuid` — the staff user who resolved it (NULL for admin path).
- `audit_ref uuid` — the `finance_audit` row that resolved it.

Guard: identity/terms immutable once `issued`; `applied`/`reversed` terminal.
A `draft` row may be edited (it's the unresolved half of a refund decision).

Wait — simpler: keep `oxsum.credit_notes` as-is (immutable, applied-on-write)
and put the T17 lifecycle on `oxsum.refund_corrections`? No — the spec says
`credit_notes` carries the state machine. But a draft/issued note with no
ledger entry yet needs `entry_id` NULL. Migration: `entry_id` becomes NULLable,
new columns added, `state` defaults `applied` for existing rows.

### `oxsum.refunds`
```
refund_id          uuid PK
order_id           uuid NOT NULL REFERENCES payment_orders(order_id)
note_id            uuid REFERENCES credit_notes(credit_note_id)  -- set at resolve
organization_id    uuid NOT NULL REFERENCES customer_profiles(organization_id)
business_id        uuid NOT NULL
amount_minor       bigint NOT NULL CHECK >0            -- credit-minor
display_amount_minor bigint NOT NULL CHECK >0          -- fiat the PSP returns
display_currency   text NOT NULL
reason             text NOT NULL
state              text NOT NULL DEFAULT 'requested'
  CHECK state IN ('requested','declined','provider_pending',
                  'succeeded','failed','released')
provider_refund_id text                                -- PSP's refund id
provider_payment_id text                               -- the payment it refunds
retry_count        int NOT NULL DEFAULT 0
next_retry_at      timestamptz
requested_by       uuid                                -- customer user (nullable)
resolved_by        uuid                                -- staff user
audit_ref          uuid
request_key        text NOT NULL                       -- customer idempotency key
UNIQUE (organization_id, request_key)
UNIQUE (merchant-scoped provider_refund_id) → partial idx on provider_refund_id
created_at / updated_at
```

`refundable` bound enforced in code: `sum(refunds in
provider_pending/succeeded for order) + new <= order.amount_minor`. The promo
leg (`order.promo_minor`) is never refundable cash — a bundle refund cancels
the lot (T18 seam: `Wallet::burn_promo_in` when promo_lots lands; today the
bundle promo is `wallet.adjust` into BONUS, so refunding purchased credit only
draws the WALLET pool — see "Promo" below).

### `oxsum.disputes`
```
dispute_id         uuid PK
order_id           uuid NOT NULL REFERENCES payment_orders(order_id)
organization_id    uuid NOT NULL
merchant_id        uuid NOT NULL
provider_dispute_id text                               -- event.refund_id reuse? no —
                       provider names the dispute by its payment/case id; keep `refund_id`
                       field carried in the normalized event
amount_minor       bigint  -- display_minor of the charge disputed
currency           text
state              text CHECK IN ('opened','evidence_due','won','lost','closed')
recharge_entry_id  uuid                                -- dispute-loss re-charge entry
inbox_receipt_id   uuid REFERENCES merchant_event_inbox(receipt_id)
created_at / updated_at
UNIQUE (merchant_id, provider_dispute_id)
```

### `oxsum.finance_audit`
```
audit_id           uuid PK
business_id        uuid NOT NULL
actor_user_id      uuid NOT NULL         -- the staff user
action             text                  -- 'refund.approve' | 'refund.decline' | 'dispute.resolve'
target             text                  -- refund_id / dispute_id
detail             jsonb
note_id            uuid                  -- the customer-visible note it produced
idempotency_key    text
created_at         timestamptz
UNIQUE (business_id, action, idempotency_key)
```

## Flows

### Customer refund request
`POST /api/v1/orders/{id}/refund-request` `{idempotencyKey, amountMinor?, reason}`
- amount in **display** minor? Decide: credit-minor is the ledger term the
  wallet books; the PSP refund is issued in `display_amount_minor` scaled
  pro-rata. Simpler and honest: the request names the **credit** amount
  (`amountMinor`, credit-minor) bounded by `order.amount_minor − refunded`.
  The PSP call converts back at the order's pinned rate: `display_refund =
  floor(display_amount_minor × credit_refund / amount_minor)`, and the last
  (full) refund takes the remainder so rounding never strands a cent.
- Creates `refunds` row `requested`. No PSP, no ledger. Replay returns the row.

### Staff resolve
`POST /api/v1/business/refunds/{id}/resolve` `{idempotencyKey, decision: approve|decline, reason?}`
- `decline` → state `declined` + finance_audit + (no credit note).
- `approve` → in one tx: credit_note row (`issued`, entry posted — the
  customer-visible wallet credit: `debit Revenue, credit Wallet`), refunds row
  → `provider_pending` with `next_retry_at = now()`, finance_audit row. The
  credit note is the customer-visible half — the wallet already shows it
  "returned". The PSP refund is the cash half and may still fail.

Hmm — should approve book the wallet credit immediately, or only after
`refund_succeeded`? Spec: "Credit note original funding reversal" and "reserve
before PSP call". A credit note is a books fact (D12: moves no money). Booking
it at approve makes the customer whole in credit terms immediately; the
refund's job is returning the cash the platform still holds. If the PSP refund
definitely fails → `released`, the note is `reversed` (a new ledger entry
re-debiting? or marked unreimbursed?). Decide: on definite failure, refund →
`released`, note → `reversed`, and a compensating `wallet.reverse`-style
debit — a `debit Wallet, credit Revenue` entry taking the credit back —
under key `refund-release:<refund_id>`. That honors "definite-failure
release" and never goes negative (FundedReservations may refuse → then the
note stays `applied` and refund → `failed`? No: pick the deterministic path —
release books the take-back entry; if InsufficientFunds, the reservation
releases anyway and the audit row records `compensation: insufficientFunds` —
the wallet can't go negative, so the uncompensated note is the audit's job).

Simpler still, and matching "reservation": don't book the wallet credit at
approve at all. The reservation is: `refunds` row `provider_pending` holds
`amount` against the order's refundable bound (a SQL-level bound via the
unique/sum check), and the PSP is called. On `refund_succeeded` webhook →
credit note issued (`applied` — wallet credited), refund `succeeded`. On
`refund_failed`/timeout → `released`, bound freed. On provider timeout at
issue time → `provider_pending` persists, `refund_retries` job re-issues.

That makes the credit note the *outcome* record, not a pre-booked promise —
cleaner accounting: wallet credit follows actual PSP success, never needs
taking back. "Reservation" = the refundable bound is consumed while pending.
The spec's "credit note original funding reversal" still holds: the note
reverses (part of) the funding the order booked.

BUT: "dispute policy distinct from successful refund" and the test "provider
timeout" — this design handles both. And "failed compensation" test = promo
lot cancel or InsufficientFunds on the take-back — with outcome-first design
there's no take-back; compensation is moot. The spec explicitly lists "spent/
promotional funds and failed compensation" — the wallet-credit-first design
gives that a real test: approve books the credit, customer spends it, PSP
refund fails → release tries the take-back debit, wallet pool is spent →
InsufficientFunds → refund `released` with `compensation_failed` audit note,
never negative. That is the harder, more honest design — take it.

Final design = wallet-credit-first (reservation books the credit note's
ledger entry at approve; definite failure issues the compensating debit, and
insufficient funds is recorded, never negative).

### PSP call
`oxsum_integrations::merchant_checkout::create_refund(context, order_binding,
refund_id_uuid, display_amount_minor, reason, http)` — per rail:
- hyperswitch: `POST /refunds` `{payment_id, amount, reason, metadata:{refund_id}}`
- stripe: `POST /v1/refunds` `{charge/payment_intent, amount, metadata[refund_id]}`
- airwallex: `POST /api/v1/pa/refunds/create` `{request_id, payment_intent_id, amount}`
Return `provider_refund_id` + status. Refund id = `refunds.refund_id` UUID as
provider idempotency key.

`merchant_checkout::create_refund` is the new seam; stub-friendly via
ProviderHttp + api_url like the checkout stub.

### Webhook application (`apply_order_event` family)
Extend `Db::apply_order_event` (core) — receipt kinds `refund_succeeded` /
`refund_failed` / `dispute` currently fall through to `_ => {}`. Add:
- `RefundSucceeded`: find refund by `(merchant, event.refund_id)` → terminal
  `succeeded`; if refund already `provider_pending` and wallet credit not yet
  booked (defensive), book it; mark event applied.
- `RefundFailed`: refund → `released` (definite failure), issue compensating
  debit if the credit note stands `applied`; event applied.
- `Dispute`: create `disputes` row `opened` deduped on
  `(merchant_id, provider_dispute_id)` where provider_dispute_id =
  `event.refund_id.unwrap_or(provider_payment_id)`; event applied.
Also `merchant_events.rs` server `apply_order_event` already routes
order-purpose bindings to `db.apply_order_event` — same entry point; refund
events name the ORDER id via metadata.payment_id (the refund's metadata
carries our refund_id + the original payment metadata? — the PSP copies our
refund metadata; the *payment* object the event wraps carries the order's
original `payment_id` metadata, so `receipt.payment_id` = order_id. Good:
`classify` already binds it to the order binding).

Retry job: new kind `refund-retries` in jobs.rs — unleadered like POLL_TASKS
(rows are self-serializing via `FOR UPDATE`), interval 60s. Sweeps
`refunds.state='provider_pending' AND next_retry_at <= now()`, re-issues PSP
call, `retry_count++`, backoff via `next_retry_at = now() + delay`; past
MAX_RETRY (5) → `released` + compensation. The synchronous resolve path also
attempts the first call inline (provider_pending persists on timeout).

### Dispute lost re-charge
`POST /api/v1/business/disputes/{id}/resolve` — staff decision `won|lost`? A
dispute's outcome really arrives from the PSP — but our normalized event only
carries `dispute_opened`. So: `opened` rows record; a `dispute_closed`/
`dispute_won`/`dispute_lost` webhook kind isn't in the normalizer yet — add
`dispute_won`/`dispute_lost`/`dispute_closed` to hyperswitch kind mapping →
`MerchantEventKind::Dispute` with a `dispute_outcome` field? Keep it minimal:
extend `VerifiedMerchantEvent` with `outcome: Option<String>` populated from
the object (`dispute_status`/`status`), and `Db::apply_dispute_event` moves
`opened→evidence_due`? Spec states `opened → evidence_due → won/lost/closed`.
Map: `dispute_opened` → `opened`; subsequent dispute events with status text
move through `evidence_due`/`won`/`lost`/`closed`. `lost` triggers
`Wallet::repay_credit`? No — re-charge means taking money the org was
refunded: a lost dispute means the customer *kept* the refund and the
platform clawed nothing — wait, lost dispute = customer wins chargeback =
platform loses the cash. The re-charge spec says "a lost dispute re-charges
through `Wallet::repay_credit` under a distinct entry kind". So on `lost`:
post `repay_credit`-keyed entry `debit Cash, credit CreditLine`? That repays
a drawn line — semantically the platform's cash leaves to the card network,
restoring nothing... Actually `repay_credit` semantics: `debit Cash, credit
CreditLine` — cash out, line restored — that's exactly "customer's chargeback
took cash, the credit they drew is now owed again"? Hmm, spec says reuse it
so the ledger reads the reversal as debt-restoration, distinct from a refund
(which credits wallet). I'll implement `Wallet::dispute_recharge` — a
pub(crate) entry `debit Cash, credit CreditLine` capped at drawn (falls back
to `Revenue` credit when nothing drawn? — no, keep repay_credit semantics:
refuse when nothing drawn? A lost dispute when the org never borrowed would
fail — that can't be). Better: `debit Cash, credit Revenue` — money leaves,
income un-recognized — a "chargeback" entry kind. The spec explicitly says
`Wallet::repay_credit` "under a distinct entry kind" — repay_credit already
caps at drawn which fails otherwise; the distinct entry kind suggests a new
method that reuses the repay *shape* but debits the WALLET pool... Re-read:
"a lost dispute re-charges through Wallet::repay_credit under a distinct
entry kind so the customer-visible ledger doesn't confuse reversal with
refund". So the customer is re-charged — they owe again. `repay_credit`
moves `debit Cash, credit CreditLine` — that models "the platform repaid the
line with fresh cash". For the org to owe again, the entry should restore
the drawable line — which is what repay_credit does when there's a draw.
When nothing is drawn, the platform lost cash to the card network and the
org owes... wallet debit would make them pay again = negative balance risk.

Given ambiguity: implement `Wallet::dispute_recharge(key, reason, minor, on)`
= mirror of the funding leg, `debit Wallet, credit Cash`?? No — that debits
the customer's wallet (taking their balance) = the "re-charge": the refunded
amount is taken back from their purchased balance, cash account credited
back because the bank clawed it to the network — wait the bank clawed cash
OUT, so cash decreases → `credit Cash`. And the customer re-owes → their
purchased wallet liability should INCREASE? No — they got refunded money as
wallet credit; losing the dispute (from the platform's view, customer wins)
means... "lost dispute" from merchant side = chargeback succeeded = customer
keeps money AND the order's original charge is reversed at the network. The
re-charge then = the platform's counter-entry: `debit Cash(clawed), credit
???`. Ledger truth: cash decreased by the chargeback; the wallet credit the
refund gave stays OR gets taken back.

I am over-thinking an ambiguous spec line. Pragmatic implementation: on
`lost`, debit the wallet pool back (take the refund's credit away — customer
got their money back at network level, they don't keep double credit) —
`debit Wallet, credit Cash` is wrong direction (that's taking wallet balance
and crediting cash = increasing platform cash). Correct mirror: the refund
had `debit Revenue, credit Wallet` (credit note) — wait no: my design's
refund cash-out is `debit Wallet, credit Cash` when the PSP succeeds (take
back the wallet credit, cash left). Let me FIX the ledger story cleanly:

- Order fulfillment (topup): `debit Cash, credit Wallet` — cash in, customer
  credit up.
- Refund reservation (approve): book NOTHING to wallet. Reserve bound only.
- refund_succeeded webhook: `debit Wallet, credit Cash` — customer credit
  down, platform cash out. PLUS the `credit_notes` row recording the
  correction (note_id, order_id, entry_id, reason, `applied`). The credit
  note IS this entry — "credit note = original funding reversal".
- refund_failed/timeout→released: nothing to undo.
- This gives clean invariant: wallet can never go negative because
  `debit Wallet` is FundedReservations-guarded — customer spent the credit
  already → InsufficientFunds → refund stays... it can't proceed? But PSP
  already refunded! Money's gone at network; the take-back fails → THAT is
  "failed compensation": entry retries/waits; the dispute table/audit
  records it; wallet never negative. The refund row → `succeeded`
  regardless (PSP did refund); the note stays `issued` (unapplied) and a
  retry pass applies the wallet debit when balance allows?? No — money left
  already; the wallet debit is owed forever: keep refund `succeeded` +
  `compensation: pending`, retry in sweep, audit row notes the exposure.

This is the honest Blnk/Lago shape: refund completion is PSP-fact; the
ledger compensation is best-effort and audited when the customer spent the
credit. Wallet can never go negative. I'll take THIS design (outcome-first,
compensation-tracked) — it satisfies "no silent negative wallet",
"failed compensation" test (compensation pending + audit records it), and
avoids the take-back-of-already-spent-credit paradox the credit-first design
creates.

So credit_notes.state: `draft|issued|applied|reversed`:
- created `issued` at refund approve (recorded, entry not yet posted — entry_id NULL).
   Wait, keep it simpler: note row created at approve WITHOUT ledger entry
   (state `issued`), `entry_id` filled when the wallet debit lands
   (`applied`); `reversed` if staff reverse it... no reversal path needed:
   refund released → note `reversed` (the correction never applied, the
   record stands as audit).

Promo: bundle promo leg is bonus-pool today (`wallet.adjust`), T18 moves it
to promo lots. Refund bound = `order.amount_minor` only (promo_minor never
refundable — spec point 10: promo refunds as lot cancellation, which is
T18's seam; my code notes `remaining_promo = order.promo_minor` is excluded
from the refundable bound, and when promo_lots exists the cancellation hook
is `Db::cancel_promo_lot` keyed `refund:{refund_id}:promo` — I'll wire the
call behind a `promo_lots`-table-exists check? No — T18 isn't merged. I'll
put a `refund_promo_lot` TODO hook: `#[cfg]`? Cleaner: leave the call site
marked `// T18 seam: cancel the bundle's promo lot here` and bound excludes
promo. Document in PR description.)

## Endpoints (openapi.yaml first)
- `POST /api/v1/orders/{orderId}/refund-request` — customer, session or key? A
  refund request is a person's action → session cookie only? Spec says
  "customer can request review" — allow sessionCookie + apiKey (like order
  create). Body `{idempotencyKey, amountMinor?, reason}`.
- `GET /api/v1/orders/{orderId}/refunds` — customer reads its order's refunds.
- `GET /api/v1/business/refunds` (+ `{id}`, `{id}/resolve`) — staff
  RefundsWrite / ReportsRead for read.
- `GET /api/v1/business/credit-notes` — staff finance read.
- `GET /api/v1/business/disputes` (+ `{id}`) — staff read; resolution is
  webhook-driven, plus manual `POST {id}/resolve` for `closed`.

## Tests (crates/server/tests/refunds.rs + core coverage)
A18/A19 concurrent consume/refund — spawn racing `wallet.hold`+settle loop
vs refund request/approve on same org; assert no negative, bound holds.
Partial total — two partial refunds sum ≤ order.
Duplicate/reordered webhook — refund_succeeded twice / failed then succeeded.
Provider timeout — stub hangs → provider_pending → job retries → succeeds.
Spent funds — refund credit spent → compensation pending, wallet ≥ 0.
Promo — bundle order refund bound excludes promo_minor.
Failed compensation — audit row records it.
