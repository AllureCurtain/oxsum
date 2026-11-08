# Versioned daily plan recognition (#838)

## Policy and financial behavior

Previously every sold plan used the retained policy that recognized its full fee
when the period closed. Migration 0113 leaves those published versions and
authorized periods on policy 1. New publications use policy 2; ordinary renewal
of a policy-1 version remains policy 1 until the customer explicitly selects a
new version and pays for its activation.

A policy-2 activation immediately issues the paid invoice and statement while
the collection remains deferred. The immutable accounting period records the
original version, fee, authorization, exact UTC service interval and payment
date. Completed UTC days recognize a cumulative share
`floor(net_fee * elapsed_microseconds / duration_microseconds)` with checked
integer arithmetic. The last interval receives the exact remainder. Backlog
books on the current UTC date and retains its original service-day attribution;
the worker cannot use a caller-supplied future clock.

Refunds catch up completed days, reduce net consideration and reverse the excess
earned amount through Discounts; the remaining reduction consumes the original
deferred fee. Lost disputes preserve service revenue and restore the original
receivable, with excess external cash in review suspense. Financial void reverses
the net earned/deferred amount and moves collected consideration to RefundPayable
without paying cash. Quota and entitlement never become refundable cash. Period
close uses the activation invoice and does not recognize or bill it again.

Reports separately expose billed, earned, deferred, collected, refund, discount
and receivable amounts. The conservation identity is
`billed - collected = receivable`; a prepaid invoice is not earned revenue.
Daily effects carry separate booking, service and payment clocks. Filtered daily
facts are movements, whereas retained policy-1 journals keep their documented
posted/issued cohorts. A confirmed policy-1 refund and its matching journal
discount now contribute one cash return instead of two.

## Opening, action and closing conservation

The production PostgreSQL/ledger tests assert these positions for a fee
`F = 2,500,000` minor units and a partial refund/dispute `R = 1,000,000`.
`E(F)` is the checked cumulative completed-day share at the action boundary.
Cash and receivables below use positive asset positions, rather than the tests'
credit-minus-debit helper. Income is net of the Discounts contra. Each row is an
independent scenario unless it explicitly follows the preceding action.

| Position | Cash | Net earned | Deferred | Receivable | RefundPayable | Net billed / collected |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| Paid activation | F | 0 | F | 0 | 0 | F / F |
| Completed-day catch-up | F | E(F) | F - E(F) | 0 | 0 | F / F |
| Partial refund after catch-up | F - R | E(F - R) | F - R - E(F - R) | 0 | 0 | F - R / F - R |
| Service close after that refund | F - R | F - R | 0 | 0 | 0 | F - R / F - R |
| Full refund | 0 | 0 | 0 | 0 | 0 | 0 / 0 |
| Partial lost dispute | F - R | E(F) | F - E(F) | R | 0 | F / F - R |
| Financial void | F | 0 | 0 | 0 | F | 0 / 0 |
| Partial refund after void | F - R | 0 | 0 | 0 | F - R | 0 / 0 |

Partial and full refunds, voids and lost disputes run at activation, during
service and after the service interval ends. Repeated verified events do not
repeat cash. A late success after a reversed refund note still records the
observed cash exactly once. Mixed policy statements are exercised in both
directions, including a prior partial refund, a larger subsequent chargeback,
remaining original debt, and suspense for cash exceeding that debt.

## Evidence integrity and recovery

Policy-2 statement artifacts capture their accounting inputs and effects in the
same repeatable-read snapshot as the statement header, lines and invoice. The
capture has an RFC3339 timestamp, and every non-null accounting effect entry ID
must have a verified included ledger entry. Existing stored export bytes remain
unchanged. This proves captured source integrity and inclusion within the
documented scope; it does not prove delivery, statutory compliance, or absence
of later accounting effects.

Independent review found a race between recaptured artifacts and the original
deterministic ledger marker, and a crash after marker append could make the
same scope permanently inconsistent. An immutable `evidence_mint_inputs` row
now chooses the first complete collector before marker append. Contenders and
retries reload that winner, derive the same manifest, and return the actual
persisted bundle descriptor. The marker verifies its exact domain and digest.
Only an idempotency conflict permits a raced reread; unrelated errors propagate.
No transaction or connection remains held across nested ledger API calls.
An older orphan marker without matching captured inputs refuses publication.

The final review of this evidence path passed after execution of concurrent
mint/refund, four mints sharing one pool connection, forced export failure after
marker append for both policy versions, retry after a later refund, exact single
effect-entry omission and orphan-marker mismatch tests.

## Development validation

The final focused runtime/test snapshot was `65aad22` on integrated main
`23b6547`. It was subsequently rebased onto business-readiness main `df4ad4f` as
`42e9586`; only the TODO status merge required conflict resolution, with both
workstreams retained. Final integration rebases retained #840 and #850, producing
runtime/test snapshot `561bfb590a011f1c626ea806550efc41c076329f` on main
`5dc49dc969965ab2f992b70cbef390fd01821748`. Range-diff confirmed the runtime and
test patches were unchanged; conflicts only joined progress and policy documents.

| Check | Observed result |
| --- | --- |
| Daily accounting, arithmetic, worker and evidence faults | 25 passed in 9.008 seconds; 113 unrelated lib tests filtered |
| Core legacy financial, lifecycle, payment, dispute and upgrade selection before the final evidence refinement | 135 passed, zero skipped, in 30.164 seconds |
| Final HTTP plans, journals, statements, business console, payments, facts and evidence | 49 passed in 11.089 seconds; one existing fixture-regeneration helper ignored |
| Final migration inventory/concurrency and pre-0109 payment upgrade | 12 passed, zero skipped, in 2.367 seconds |
| Pure verifier | 93 passed, zero skipped |
| Strict workspace/all-target Clippy | Passed with `-D warnings` in 21.37 seconds |
| Browser-compatible verifier | `wasm32-unknown-unknown --features wasm` passed in 66 seconds |
| Contract/generated frontend | Four OpenAPI security checks, generated types and production build passed |
| Formatting and patch integrity | `scripts/fmt-check.sh` and `git diff --check` passed |
| Complete integrated workspace | 1,995 passed, zero failed, six existing opt-in skips across 192 binaries in 292.685 seconds |

The complete run used explicit local `DATABASE_URL` and
`DOUBLEENTRY_DATABASE_URL` and `cargo nextest run --workspace --no-fail-fast`.
Run ID: `b4baef5f-5d53-4297-8411-5af0e6690bc1`. Every financial hypothesis and the
PostgreSQL ledger conformance suite ran. The six skips are two fixture-generation
helpers, two real-provider cases, the optional load check and reference capacity.
The final runtime also passed the five required PR checks: fmt, strict Clippy,
lib/doc tests, wasm and frontend. Evidence/progress documentation alone changed
after this full run; normal protected integration still requires the final
commit's required CI.

The daily selection covers one ledger entry per completed day, replay, final
close, transaction rollback after a ledger append, skipped locked candidates,
refund/recognition and refund/close races, frozen period terms, legacy ordinary
renewal and explicit policy transition, free periods, service/booking/payment
filters, sealed prior-month catch-up, partial first/last days, maximum integer
arithmetic, and real 1/17-minor fees over the 2028 January-to-February leap-month
interval. The initial leap fixture used 2024, before the ledger's fixed 2026
opening date, and correctly failed account-date validation; the fixture was
corrected without weakening production date constraints.

## Integration and remaining scope

Drain old server and worker versions before activating policy-2 writers; an old
refund worker does not understand the daily earned/deferred allocation. Use the
normal single-writer upgrade procedure and retain original exports. Migration
0113 does not rewrite committed ledger history or stored evidence.

PR #851 carries the completed combined validation and source review; normal
protected integration is pending at this checkpoint. Runtime independent
reconciliation is explicitly tracked by
[#848](https://github.com/AllureCurtain/oxsum/issues/848): the current pass still
clones the financial projection into `actualTotals`. The independent ledger
assertions above are development verification and do not close that deployed
behavior gap. Parent N11/#737 remains open.

No provider call, PSP call, production historical repair, payout, deployment or
publication occurred. Browser journeys, exact candidate image acceptance and
external certification retain their separate gates.
