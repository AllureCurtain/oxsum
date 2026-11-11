# Business monthly statement inspection (#1007)

Base: `2ee706019a02f24b4b8ed4b949b8c5254b27a178`. Parent #1006 / N10 / J11.

## Delivered scope

Personal Business reports authority can list one customer's monthly wallet-credit
statements and inspect its original lines, issue terms and proof-window identifiers.
The bounded endpoints and bilingual customer card remain separate from sold-plan
statements. List pagination uses immutable ascending UUIDs (20 default, 100 maximum);
detail refuses over 1,000 lines or 2 MiB of expanded serialized line data. A read
never initializes a ledger, reconciles payment globally, modifies original rows or
mints evidence. Current staff authority is rechecked after releasing the coherent
snapshot, including revocation that completed while the read waited for a lock.

Recorded monetary projections stay explicit. An elapsed due date displays overdue
for a pending unpaid finalized bill without writing status/timestamps. Paid and
suspended standings are preserved. Issuance/finalization/repayment remain #1006;
this read slice does not close the monthly commercial journey.

## Validation

- 27 related real PostgreSQL core/HTTP statement cases passed. Five final focused
  cases passed, covering single-connection reads, no ledger bootstrap, byte-for-byte
  stored-row preservation, original terms/anchors, page/filter boundaries, customer
  and machine rejection, cross-customer ID pairs, roles, revocation races, line
  bounds and compressed oversized text.
- The compressed-text case exposed that `pg_column_size` can measure compressed
  storage. The final guard counts expanded serialized row text and the regression
  passes; no silent truncation occurs.
- One complete workspace run executed 2,261 tests: 2,260 passed, the known baseline
  manual-bundle automatic-refund fixture failed, and six existing opt-ins skipped.
  The existing repair is PR #975; no new failure occurred. This is development
  evidence, not a green integrated release candidate.
- Core/server all-target strict Clippy, pinned formatting, six OpenAPI credential
  checks, regenerated types and production frontend build passed. The final mobile
  presentation adjustment needed only a frontend rebuild and browser inspection.

## Browser evidence

The task-owned local credit fixture database was reused through an isolated
loopback runtime at port 18807, with task space 2. Twenty-two explicitly synthetic
historical monthly document rows and two lines per document exercised reads and
pagination; they do not represent genuine invoicing, payment or proof production.
No provider, PSP or external mail was configured.

Finance browser checks observed 20 + 2 rows, exact detail amounts (1.000001 total,
0.700001 credit-funded, 0.2 recorded repayment, 0.500001 outstanding), original
14-day terms and ISO UTC dates. Chinese desktop and English 390×844 layouts were
inspected. Proof identifiers remain absent for the synthetic legacy fixture and
are labeled as recorded identifiers, not independent verification. The proof
section opened with keyboard Enter. The mobile detail table preserves complete
amounts through horizontal scrolling instead of splitting decimal digits.

After browser reads all 22 stored documents remained `pending`, all `overdue_at`
values were null, and recorded repayment summed to the original 4,400,000 minor
units; the UI's overdue state was purely presentational. The final binary/frontend
hashes, screenshots and sanitized observations are retained in the ignored local
acceptance directory. Independent review, protected integration and final-candidate
acceptance remain.
