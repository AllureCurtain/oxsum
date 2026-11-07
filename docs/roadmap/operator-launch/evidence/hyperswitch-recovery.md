# Hyperswitch payment and order-refund recovery evidence

Date: 2026-11-08. Issue: #760; parent N10/#736. Baseline: `0314cbd3d5439adbc904a327a8e16704b344a0af`.

## Result and scope

New operations persist body identities before provider I/O. Exact pinned-version
reads recover a committed payment or refund after its HTTP response is lost;
unknown results keep the refund reservation and issued credit note after five
leased attempts. Historical writers/rows cannot acquire a new create identity.
A supplied unknown or another order's refund ID cannot choose a reservation.
Unresolved inbox events yield to later work rather than monopolizing the page.

The separate deployment-admin deposit reversal is tracked by #763. This evidence
does not certify that path, a real PSP, browser checkout, automatic callback
transport or the final integrated candidate.

## Executed validation

- `scripts/fmt-check.sh`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo nextest run --workspace --no-fail-fast`: **1,818 passed, 6 opt-in tests
  skipped**, 217.934 seconds of test execution. The skipped external/process
  opt-ins remain final-candidate gates where applicable.
- Focused coverage includes committed-response loss, an actual ten-second HTTP
  timeout, repeat/duplicate creates, historical payment/refund fencing, old
  metadata compatibility, cross-order and unknown refund IDs, exact signed
  amount/currency/customer checks, concurrent claims, exhaustion with reservation
  and note retained, and fairness across 101 unresolved inbox receipts.
- `cargo build -p oxsum-server`: passed.
- `scripts/hyperswitch-recovery-smoke.py` against #757's pinned v1.129.0 stack:
  passed with the exact debug binary SHA-256
  `7b60c71eb1fcf5f4a29d3f4aad9845b4f7c6b80371ea5b7ed6ea1cb84d9ffc30`.

The switch smoke used its dummy connector and locally signed callbacks. Payment
`67bd427c-20f1-4ee7-9cee-5c3b59797228` and refund
`d4375f91-1755-4e5f-90bd-4eedf8ec41ab` each received one create POST despite two
dropped responses overall. There were two payment reads, three refund reads
and two subsequent repeat reads. A separate explicit duplicate refund received
`HE_01` without changing its identity or amount. USD 2.00 paid and USD 0.50
refunded at 1 credit = USD 1 left exactly 1,500,000 credit-minor and one applied
linked credit note; pending, duplicate and bad-signature events produced no
extra credit. The task's isolated listener/proxy and scratch database were
removed by the runner; the pinned stack and its persistent volumes remain.

Local retained logs are under ignored `local-notes/operator-launch/`:
`recovery-final-{fmt,clippy,workspace,build,switch}.log`. The isolated fixture
result is `.runtime/smoke-e35ff27c2c6c/result.json` in the #757 worktree. Earlier
runs and one corrected repeated-setup test fixture are retained there as well;
the final workspace result above covers the corrected code.

## Remaining acceptance

Genuine reviewer approval and protected integration are pending. Full exact-SHA
candidate CI, automatic public-HTTPS callbacks, browser checkout and a genuine
PSP sandbox are separate gates. No real-model attempt or charge was made.
