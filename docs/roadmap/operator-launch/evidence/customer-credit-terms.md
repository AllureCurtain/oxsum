# Monthly customer credit terms (#997)

Development base: `2ee706019a02f24b4b8ed4b949b8c5254b27a178`.
Issue: [#997](https://github.com/AllureCurtain/oxsum/issues/997).
Scope: N10/J11 monthly wallet facility approval; monthly statement management/payment is separate.

## Implemented behavior

The personal Business customer detail card and GET/PUT credit-terms contract
separate monthly wallet credit from sold-plan receivable limits. Reports readers
inspect; owner/finance approve an exact total, future invoice payment terms,
nullable monthly dunning grace and reason. The existing facility ledger effect,
organization terms, immutable mutation receipt and actor audit commit together.
No-op decisions retain a receipt; old retries never overwrite newer terms.

Customer, explicit machine (including alongside an owner cookie), support,
foreign and archived-write boundaries fail closed. Reads do not initialize a
ledger. Single-connection reads/writes avoid nested pool checkout. The coherent
read snapshot is followed by a fresh authority check to refuse a staff revocation
that completed while the snapshot waited for the Business lock. Empty bootstrap
may precede the final write transaction, but no credit/terms/audit commits before
its current authority/archive checks.

## Verification

- 58 affected real PostgreSQL core/HTTP, wallet, monthly-statement and dunning
  cases passed before the final additional authority/issued-document checks.
- All eight final focused core/HTTP cases passed, covering no-op and conflicting replay, injected
  failure after successful ledger append, concurrent exact retry, reserved and
  drawn shrink refusal, current staff roles, archived customers, one-connection
  operation, manual-suspension preservation, immutable issued terms/anchor,
  revoked replay and a controlled read-versus-revocation race.
- One complete cross-crate workspace run executed 2,262 tests: 2,261 passed,
  one known baseline bundle-upgrade test failed, and six existing opt-ins were
  skipped. The failure expects an automatic PSP refund for a manual receipt;
  pending PR #975 fixes that fixture. Applying only its test change temporarily
  to this feature tree made that exact test pass (1/1); the temporary change was
  removed. This is combined development evidence, not a green merged candidate.
- Changed core/server strict Clippy, pinned formatting, six OpenAPI credential
  checks, exact code generation, three frontend money-input checks and production
  frontend build passed. Relevant focused checks were repeated after the final
  read-authority change; no second full run was needed.

## Browser acceptance

An isolated `oxsum_credit_browser_997` PostgreSQL database and loopback server
at port 18797 used production setup/login routes, a fresh owner and customer,
no provider/payment/mail configuration and task space 2. The built feature UI:

- Preserved and confirmed `1.000001` credits, 14 days and 3 days grace in Chinese.
- Committed `2.000002` credits and deliberately lost the successful HTTP response.
  Closing/reopening retained four locked fields; retry sent byte-identical JSON,
  resolved the original receipt and unlocked inputs. Only one decision was
  recorded for those two attempts.
- Reserved 1.5 credits through the production Wallet API in the isolated fixture.
  The UI refused a shrink to 1 credit plus changed terms, preserving 2.000002
  credits/30 days/null grace and recording no rejected mutation. The input then
  allowed correction to 3.000003 credits/45 days, which succeeded.
- On the final development build, a finance session submitted an unchanged-limit
  approval successfully. Changing its role to support caused the live capability
  refresh to remove the monthly credit card; the pending click was disconnected
  before submission. Current terms remained unchanged. HTTP tests cover direct
  stale-authority submissions.
- Rendered English refusal and Chinese success at 390×844 with document width
  equal to viewport width; inspected screenshots show readable confirmation and
  field layout. Exact server values and mutation counts were checked separately.

Private credentials and transient runtime artifacts remain outside the repository
under `/tmp/oxsum-credit-browser-997`; screenshots and sanitized results are retained
there and under ignored `local-notes/operator-launch/customer-credit-997` during continuation. Final candidate/browser certification, independent
review and protected integration remain. No external cost or publication occurred.
