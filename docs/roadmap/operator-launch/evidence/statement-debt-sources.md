# Exact statement debt source mapping (#917)

Parent #777 / #736 / #725. Base `5285fc0b`.

A statement's wrapping journal is insufficient for payment allocation. Existing
lost-dispute processing appends restored debt to the next open statement, or
creates a new chargeback-only statement beside the original paid document.
This slice maps each unpaid source to its original journal before a future
repayment quote/reservation may use it.

The core reader enforces current personal customer scope and hides foreign IDs
before capturing data. One read-only repeatable-read transaction reuses the
bounded canonical financial comparison for source, line, payment, refund,
recognition and official-entry consistency. The exact dispute/order/period
mapping and verified receivable debit establish each chargeback slice; duplicate
sources, inconsistent cross-document totals and ambiguous allocation refuse.
Amounts remain integer credit-minor. Cancellation does not remove an old debt.
An uncorrected historical usage-overage journal requires explicit source review
even when its recorded AR balances: old binaries could invoice already-settled
usage twice. New ordinary fee activations are fully prepaid, so no synthetic
unpaid fee is presented as a production-created source.

A verified response contains exact per-journal remaining amounts and original
line/dispute identities plus a stable source revision. A review-required response
keeps the displayed outstanding amount but supplies no guessed slice. Its source
scope is conservative: an unrelated inconsistent financial source in the same
customer can require review. This is explicit, not a claim of zero debt.

No new HTTP/payment purpose, provider I/O, reservation, account bootstrap or
financial mutation is introduced. #777 remains open for the complete
quote/create/reserve/apply/reverse/UI/recovery behavior, and the new allocation
writers must extend source comparison before enabling checkout.

## Validation

- Full core regression: 773 passed, zero skipped (331.923 seconds).
- Final focused financial-source regression after the historical usage refusal:
  29 passed (10.911 seconds), including exact mixed-policy/chargeback mappings,
  cancelled-plan debt, foreign and machine authority refusal, stable/no-write
  reads, official orphan entries, duplicate documents, balanced historical
  overage review and more than 10,000 source rows refusing without truncation.
- Strict core all-target Clippy, pinned formatting and whitespace checks passed.
- Reused the full regression for unchanged modules; the final refusal and test
  additions received the focused source suite and strict Clippy again.

Browser, PSP and candidate acceptance are outside this core-only prerequisite.
Independent review and protected integration remain pending.
