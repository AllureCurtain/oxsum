# Borrowed ledger source reads (#856)

## Result and scope

Transaction-owned wallet work no longer checks out another PostgreSQL connection
for its entry, calendar, sealed-day or promo-ownership reads. Credit repayment,
repaying top-ups and credit-limit changes append on the transaction holding the
credit lock. Complete canonical entry verification is shared by the standalone
source-hash reader and the transaction-bound reader. Hold amounts and attribution
come from verified postings/provenance, and the requested hold must match the raw
idempotency key because the entry's database ID is outside the content hash.

The batch reader retains individual malformed/hash-mismatch failures while
operational database errors abort the read. It does not silently replace corrupt
amounts with zero. Raw keys, document identifiers with and without hashes, full
provenance, reversal metadata, posting order, layers and dimensions survive the
round-trip. Missing schemas and entries remain absent; unsequenced entries retain
the engine's ordinary read semantics. Consumers must independently require log
sequencing where needed and bind raw keys, registry account IDs and source
ownership. The helper does not prove Merkle inclusion or business completeness,
and the caller owns its transaction and isolation level.

No migration, ledger-engine, API or financial-policy change is included. This is
a shared prerequisite for non-plan refunds (#841), independent reconciliation
(#848) and funded/free-call admission (#849), not closure of those features.

## Validation

Runtime/test SHA: `07205e843b3aaeb13518088c1dc4824f095fa434`, based on main
`8f92546d`. The final evidence/status commit changes documentation only. The
branch was then rebased onto main `b67a2d3b` after the retained-head browser fix;
range-diff and a direct `crates/core` comparison confirmed identical runtime/test
bytes. Only adjacent development-document paragraphs needed conflict resolution.

| Check | Observed result |
| --- | --- |
| New PostgreSQL regressions | 9 passed; one-slot pool, fully occupied two-slot pool, complete canonical round-trip, metadata/posting/dimension tampering, malformed documents/keys, foreign/missing sources, operational failures, repeatable-read visibility, own uncommitted append/replay/hold attribution, wrong raw-key refusal, promo grant/replay/hold/settlement, transaction-local day/period seals |
| Complete workspace | 2,012 passed, zero failed, six existing opt-in skips, 194 binaries; 342.140 seconds; nextest run `57ece966-a376-47b0-8033-0317631edd10` |
| Strict workspace Clippy | `cargo clippy --workspace --all-targets -- -D warnings` passed |
| Formatting | `scripts/fmt-check.sh` passed |
| Independent source review | Root, refund and admission reviewers passed; the hold raw-key binding finding was fixed and tested before the complete run |

The full run includes existing daily recognition, plan/non-plan refunds, credit
limits, wallets, journals, source proofs and server routes. Its fault suite passed,
including the saturated-pool fixture in 33.528 seconds. That fixture uses the same
30-second deadline as the SQLx default acquisition timeout and has a separate CI
race repair in #862; this successful run does not close that issue.

PR #860 uses normal required CI and normal merge. Its exact final checks must be
green before integration. These are development integration results, not a frozen
operator-launch candidate, real PSP/provider certification or release acceptance.
