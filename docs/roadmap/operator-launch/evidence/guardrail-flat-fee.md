# Whole-response guardrail fee refunds (#899)

The integrated `5285fc0b` baseline charged 700 minor units after a whole-response
post-call guardrail refused the answer. Giving the existing real-PostgreSQL
`a_response_block_refunds_the_hold` fixture `cost_per_request: Some(700)`
reproduced the failure: the settlement retained `["request",1,700000000]`.

The gateway now makes explicit `Charge::Nothing` authoritative for customer
flat fees. Successful zero-token usage and delivered streams remain billable.
Customer receipt usage stays zero on refusal, while the actual provider report
is retained on the attempt and prices operator cost. Missing reports stay
untracked; a refused cache hit has no new upstream attempt or provider cost.
No schema or public response fields change.

Validation on the repair:

- 17 real-PostgreSQL guardrail checks passed with zero skips. The new matrix
  exercises wallet and fully quota-funded zero-wallet customers, a previously
  charged cache source, refused cache/fresh responses, delivered streams,
  successful zero-token flat fees, exact replay, complete long reasons and
  verified receipts. Ledger availability, released holds, usage/operation
  charges, quota draw/consumption and provider attempt/cost are checked.
- 122 adjacent gateway/cache/subscription/operation checks passed, with one
  existing opt-in skipped.
- Changed-crate `cargo clippy -p oxsum-server --all-targets -- -D warnings`
  and pinned formatting passed. The final added guardrail test also passed
  focused strict Clippy and the full guardrail suite.
- All four `Charge::Nothing` call sites are whole-response/cache post-block,
  upstream refusal/unreachable, and incomplete upstream body. The latter two
  were already non-billable; estimated/client-cancelled paths do not use it.

Independent review, protected PR integration and post-merge CI remain pending.
This is local fixture evidence, not provider/PSP/browser/candidate acceptance.
