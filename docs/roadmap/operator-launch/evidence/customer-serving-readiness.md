# Personal model, sale and funding readiness (#901)

Base main: `5285fc0b`. This branch integrates pending PR dependencies #910
(application read-only sale/model helpers), #911 (immediate receipt) and #913
(funding preview) locally. Each dependency still requires independent review;
local integration does not claim a protected main merge.

## Implemented boundary

The personal customer contract replaces the old deployment-token channel probe,
raw key count and positive-wallet checklist. A bounded scan chooses a visible
live key and permitted chat model with exact effective customer sale terms.
It prefers a sample whose funding passes. The customer sees a single `Hello`
user message and requested 32 output tokens, with integer total/quota/wallet
amounts and selected key metadata. No request is sent by the check.

The sale bound uses the same conservative pricing as gateway admission,
including flat fees and applicable conditional rates. Settlement discounts do
not reduce that reservation. Supplier prices establish configured route/mode
only. Model/group/customer/alias restrictions filter the scan before its limit;
`scanLimited` prevents claims of exhaustive absence.

One read-only repeatable-read transaction owns sale, authority and funding.
Immutable wallet account identities are opened first, without bootstrap and
without occupying that transaction's pool slot; a one-connection pool works.
Absent ledgers, missing terms, pending lifecycle/expiry work, restrictions and
funding refusals stay explicit. No digest, quota draw, hold, operation or
accounting write occurs. Rate windows, policy execution and provider availability
remain separate actual-request checks. First-call completion still requires
actual canonical receipt evidence from #911.

## Validation

- 22 focused core tests passed, including new sale selection, free/full quota,
  member visibility, inactive keys, absent ledger/no mutation, bounded scan and
  one-connection-pool cases plus funding/application-read dependency regressions.
- Four HTTP tests passed: current personal authority, explicit machine refusal,
  session revocation, no bootstrap and existing immediate receipt/verification.
- Five frontend state tests, four OpenAPI security tests, generated types and
  production frontend build passed. Strict all-target core/server Clippy,
  pinned formatting and local documentation links passed.
- Complete integrated workspace: **2,182 passed, six existing opt-in skips**,
  669.571 seconds. No failures. Production source did not change afterward.
- An isolated source-built fixture started on loopback port 3310 with a real
  zero-price account/receipt and an uninitialized account. Browser execution
  could not proceed: the previously owned Ego task space 10 no longer exists;
  inventory contains only two unrelated user-owned spaces. No user space was
  taken over and no replacement was created. The fixture was stopped. Final
  browser layout, interaction, error/reload and responsive checks are therefore
  **unperformed**, not passed. #911 retains its separate earlier receipt-browser
  evidence; it does not validate this new funding page.

Independent review, normal protected integration and browser completion remain.
Provider, PSP, candidate and external pilot acceptance remain separate and
pending. No external request/budget or payment occurred.
