# Refund callback correlation evidence

Issue #759, N10/#736 and N06/#732. Development evidence observed 2026-11-08;
this is not final candidate or real PSP acceptance.

Hyperswitch v1.129.0 retains refund-request metadata rather than copying the
payment's metadata. Previous oxsum requests sent the refund UUID as `order_id`
and omitted payment/customer coordinates. A real local-switch smoke therefore
left an authenticated refund callback unable to correlate to its reservation.

The fix sends the distinct refund, order/payment and customer IDs. Missing
coordinates on historical signed events are derived only from immutable
provider references inside the exact merchant and pinned version. Supplied
conflicts and malformed IDs are rejected. Early callbacks stay pending until
the reference exists. Original normalized events and their digests remain
immutable; the database immutability trigger was neither changed nor disabled.

Executed validation:

- 25 focused core/server merchant-event/refund tests passed, including missing
  metadata recovery, conflicting identities, rotated versions, equal provider
  IDs in different merchants, digest replay, malformed UUIDs, immutable intake
  and duplicate old-shape refund settlement.
- `scripts/fmt-check.sh` and strict workspace Clippy passed; generated OpenAPI
  TypeScript declarations were refreshed.
- Full workspace nextest passed: 1,811 tests, 6 skipped, 213.500 seconds of
  test execution. The skipped tests remain real-provider/fixture-helper/load/
  sustained-capacity opt-ins, not completed acceptance. The retained log is
  `local-notes/operator-launch/refund-workspace-nextest.log` outside worktrees.
- Actual local Hyperswitch simulation through the #757 smoke: USD 2 payment,
  USD 0.50 partial refund, final 1,500,000 credit-minor balance, exactly one
  applied linked credit note, duplicate events without extra effects, and bad
  signatures rejected. The fixture uses USD 1 per credit.
  Binary SHA-256: `7a328782b26baad9a9d0df58515f244195ae83d638dfd048194486f9e66c193a`.
  Runtime result: `smoke-38b2549b26f7/result.json` in #757's private fixture.
  The isolated listener stopped and its scratch database was removed.

The switch used its in-process dummy connector and locally signed events from
actual switch responses. Automatic callback delivery, browser checkout and
real PSP certification were not performed. Provider retry identity and unknown
outcome recovery remain separate issue #760. There was no real model call or
monetary spend.
