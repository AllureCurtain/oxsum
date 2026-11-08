# Settled plan overflow is charged once

Issue [#803](https://github.com/AllureCurtain/oxsum/issues/803), child of #779
and N10/N11. This is local PostgreSQL and source-review evidence. It is not a
provider, PSP, browser or release-candidate certification.

## Behavior and retained history

New period closes bill the sold plan fee and apply its own source-linked
funding. They never aggregate already-settled wallet or credit-line usage into
another `usageOverage` charge. Those requests already have their own
`Income:Usage` entries and payment or receivable. This invariant applies to
every recognition policy; it does not depend on implementing daily recognition.

Historical usage lines and ledger entries remain immutable and readable.
Source-linked historical compensation is separately tracked by #809. This
forward repair does not silently reduce old receivables or assert that a cash
refund occurred. Tests that need old partial journals now explicitly construct
the retained historical shape through the official ledger append API.

## Real admission and settlement regressions

Both new cases activate a paid plan, consume its actual ten-credit quota
through `admit_subscription_draw` and `settle_subscription_draw`, and then admit
a three-credit overflow with zero subscription draw. The overflow takes a real
wallet hold and settlement; `record_usage` links the actual entry. One case is
cash-funded and the other draws an approved credit line. The fixture changes
only the usage timestamp to place it inside the deterministic closing window;
it never changes the settled entry.

| Fact | Before close | After close and repeated close |
| --- | --- | --- |
| Usage income | 3,000,000 | 3,000,000 |
| Recognized plan fee | 0 | 2,500 |
| Plan bill / applied payment | No closed bill | 2,500 / 2,500, paid |
| Wallet balance and receivable | Actual settlement result | Unchanged by closing a fully prepaid plan |
| Credit used, credit-funded case | 3,000,000 | 3,000,000 |
| Closed journal count | 0 | 1, including retries |

The fee fixture uses 2,500 minor units; the usage uses three credits at
1,000,000 minor units each. The different scales are intentional and expose
the duplicate charge directly. Replacing only the corrected `journals.rs`
with its preceding main version made both stronger regressions fail on the
extra 3,000,000 charge. Restoring the fix made both pass. No ignore annotation
remains on any H1–H6 financial hypothesis.

## Validation and integration boundary

Validated implementation `7871a9d` was based on integrated refund repair
`de3bbca`. The later rebase onto deployment-tooling main `941434a` produced
`eebc6e5`; the Rust, server contract and generated schema trees are unchanged
between those implementation heads.

- Focused PostgreSQL run: **69 passed, zero failed, zero skipped**, including
  the journal, statement, plan, refund-conservation and HTTP paths.
- Complete workspace run: **1,899 passed, zero failed, six existing opt-in
  skips**, 242.802 seconds. All H1–H6 cases executed.
- Strict workspace Clippy with warnings denied passed. Formatting and diff
  checks passed.
- `npm ci --ignore-scripts` and `npm run codegen` passed. All four OpenAPI
  authority checks passed; generated type changes are descriptions only.
- Independent source review found no blocker in the forward fix, real
  admission/settlement cases or historical fixture conversion.

Sanitized local logs are retained under the ignored
`local-notes/operator-launch/repair-validation/plan-usage-{focused,baseline,clippy,full,npm}.log`.
Required PR checks and normal integration remain separate from this local
evidence. Daily recognition/reporting, historical compensation #809, statement
lock repair #811 and explicit repayment #777 retain their own completion gates.
No live provider attempt or payment was made.
