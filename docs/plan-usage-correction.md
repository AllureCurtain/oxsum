# Correct historical duplicated plan usage

Use this local operation for one historical plan journal that charged settled
usage again as `usage_overage`. New-close prevention is #803; this repair is
#809, under #779. It neither returns money through a PSP nor grants wallet
credit. The already-settled usage entry and every original journal/statement
line remain unchanged.

Run from an identified checkout after migration 0111, against the intended
database with a retained backup. Supply a personal business-context session
with `FundsWrite` (owner or finance) in a private file. The command does not
discover `.env`, run migrations or lazily initialize a wallet.

```bash
DATABASE_URL='postgres://…' cargo run -p oxsum-core --example correct_plan_usage -- \
  --journal <exact-journal-uuid> --session-file /private/path/session \
  --reason 'Remove verified duplicated settled usage' --key <unique-repair-key>
```

The default is a read-only preview: no ledger, document, effect or audit write
commits. Save the JSON report. `status: proposed` means the original accrual,
usage settlements, payment allocations and customer statement agree. Evidence
includes their identities, content hashes, postings and source lines. Inspect
the opening/closing amounts and allocation before applying the same request:

```bash
DATABASE_URL='postgres://…' cargo run -p oxsum-core --example correct_plan_usage -- \
  --journal <exact-journal-uuid> --session-file /private/path/session \
  --reason 'Remove verified duplicated settled usage' --key <unique-repair-key> --apply
```

An applied correction debits `Income:PlanFees` by the duplicate amount. It
credits `Assets:Receivables` only up to the original journal's outstanding
balance; any previously collected excess credits `Liabilities:RefundPayable`.
Cash, wallet funds and the original `Income:Usage` stay unchanged. The
statement appends a negative `usage` correction. If excess collection moves
to payable, a positive `paymentApplied` line names that move and explicitly
states that cash has not been returned.

For an original fee of 2,500 and duplicate usage of 1,500, all in ledger minor
units:

| Collected against original 4,000 bill | Removed receivable | New refund payable | Corrected bill / retained payment / outstanding |
| --- | --- | --- | --- |
| 0 | 1,500 | 0 | 2,500 / 0 / 2,500 |
| 1,000 | 1,500 | 0 | 2,500 / 1,000 / 1,500 |
| 2,500 | 1,500 | 0 | 2,500 / 2,500 / 0 |
| 3,000 | 1,000 | 500 | 2,500 / 2,500 / 0 |
| 4,000 | 0 | 1,500 | 2,500 / 2,500 / 0 |

The immutable correction record, source-linked lines, ledger entry and
`plan_usage.correct` audit commit together. Repeating or concurrently applying
the same journal returns the standing effect with `alreadyApplied: true` and
does not post twice. Reusing one key with different journal/reason terms
conflicts. A correction that clears the debt removes only debt-based dunning
suspension when no other issued or past-due statement remains unpaid; it never activates or
renews entitlement.

`status: refused` and a nonempty `refusal` describe a source requiring manual
review. The CLI exits 2. Explicit apply retains that refused outcome in
immutable finance audit without changing money or documents; retrying the same
key returns it. A new reviewed attempt after correcting the source uses a new
key. Unsupported histories include void/reissue, any related refund or dispute,
tax/promo/earlier corrections, missing or inconsistent payment/usage evidence,
ambiguous statement projections and previously reversed original entries.
This bounded operation does not guess an allocation for those cases.

After apply, inspect the original journal, statement and `RefundPayable`.
Outstanding payable needs a separately authorized payout workflow. Retain the
JSON, audit, original identities and appended entry; never edit old ledger
postings, disable writer guards or issue a duplicate PSP refund to force a
repair. Daily recognition and explicit statement repayment remain #737/#777.

Every original funding order, including the authorizing order with no allocated
payment, is retained in an immutable source map. New refund requests and
approvals on those orders refuse before any provider call; review the exact
payable first. A previously confirmed late external refund or lost chargeback
still records its full Cash outflow once, with an explicit review-suspense debit
and the corrected journal IDs. It does not consume another period's deferred
prepayment, alter the corrected documents, or automatically discharge the
payable. Finance must allocate that retained cash fact before authorizing any
further payout. A later journal void adds only the corrected bill's remaining
collection to the already retained payable.
