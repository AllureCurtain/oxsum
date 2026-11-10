# Customer sale resolution and in-flight conservation

Issue #992; N04/N05, partial J03/J04. Executed 2026-11-10 in the isolated
browser acceptance database. This is local development evidence, not candidate,
provider certification or complete journey acceptance.

The owner used the real portal to add customer B's override, edit/publish draft 2,
and atomically replace the business default while customer A's stream was paused.
The owner also replaced the authorized loopback provider destination and its
credential through the connection dialog. Requests used customer keys copied
privately from their verified browser sessions and a separate Python HTTP consumer
with `max_tokens: 3`; these five calls are not claimed as Chat-page submissions.
No external inference or payment was made.

## Identities

- Backend: `ca835528c5001434ae24256c177f1adb50bed274`, binary SHA-256
  `e7929a66305d868e8aebf203498ca1bbbf1aa6d5be17ed3f3c607a794a56e117`.
- Frontend: `ad693559ff6738cb4ef4bfcdb61c4bc9ba4db657`; exact file hashes
  are in [runtime.json](runtime.json). The debug frontend includes pending #990.
- Schema: `0128_application_notifications`; macOS development host, loopback
  listener `127.0.0.1:57530`; gated model `127.0.0.1:50989/v1`.
- Customer A: `57ae06a2-47b1-4989-806b-69a8229e4569`; customer B:
  `c3dc85ec-011f-486f-87f1-e2150d2a5955`.
- Fresh current-main WASM/packed SDK from #986 was reused, with its retained
  private tarball hash in runtime.json. No older verifier build was substituted.

## Actual observations

All calls report 9 input and 3 output tokens. Integer amounts below are credit
minor units (1 credit = 1,000,000 minor units).

| Case | Resolution / admitted terms | Charge | Wallet | Quota |
| --- | --- | ---: | ---: | ---: |
| Customer B | Customer override, 1/1 credits per million | 12 | 0 | 12 |
| A paused during change | Default v1, 3.000007/4.000009 credits per million | 40 | -40 | 0 |
| A after activation | Default v2, 10/20 credits per million | 150 | -150 | 0 |
| B after default change | Same customer override | 12 | 0 | 12 |
| Interrupted provider-edit attempt | Default v2, no provider edit committed | 150 | -150 | 0 |

The initial previews independently returned A=40 and B=12. A's request
`7664910b-db6d-459d-844c-d7880792d2fb` was admitted at 09:54:14 UTC. The owner
activated v2 at `2026-11-10T09:55:35.681568Z`, then released the stream. Its
settlement `9371e0b4-0b9b-5be7-b0f9-151562dd6a64` still records v1's rates,
schedule and sale digest. The following request uses v2 and charges 150.
The original sale row retains its admitted open-ended window; the later schedule
end does not rewrite that signed row.

[conservation.json](conservation.json) checks exactly one new bill per call,
all five charges, wallet/quota split, zero reserved balance and empty open holds.
A moves 999856 → 999816 → 999666 → 999516. B's wallet remains 40000 and its
original period pays 24 quota minor across two calls. These are conservation
checks of declared fixture usage, not independent verification of provider tokens.

The owner reloaded the page, selected the same book and observed immutable v1
ended and v2 active. Submitting another overlapping default with the keyboard
returned 409 `overlapping sale windows in the same scope`; the draft and both
existing schedules stayed visible. Original response bytes are retained in
[overlap-refusal.json](overlap-refusal.json). Date-looking empty-input text in
screenshots is a placeholder; captured server responses establish actual times.

[offline-verdicts.json](offline-verdicts.json) records five successful bill,
price-head and sale-head checks against separately retained bill content hashes.
Every wrong-hash and altered-charge check fails. The consumer blocked every
network fetch (`fetchAttempts: 0`). Raw `data` slices are extracted without
round-tripping signed JSON through JavaScript numbers. The self-contained
consumer is [verify-offline.mjs](verify-offline.mjs), with [scope.mjs](scope.mjs);
install the retained exact verifier tarball in its working directory to rerun.

## UI evidence and explicit limits

Desktop screenshots capture both initial previews, changed rates/results and
conflict retention. [changed-price-mobile.png](changed-price-mobile.png) is the
actual 390px layout with keyboard focus visible on the result table. Both shell
locales were used; the sale editor itself remains English in Chinese mode.
That observed defect is tracked separately in #994 and prevents a two-locale
completion claim. Source/API identities contain no passwords or bearer secrets.

The existing browser TaskSpace 3 disappeared during the next provider in-flight
edit attempt; inventory then contained only unrelated user-owned spaces. No new
space was created. The paused local request was released without editing its
provider and settled once, with no remaining hold. Its files intentionally retain
that interrupted attempt rather than suggesting that provider-edit pinning passed.

This evidence covers default-vs-override resolution and in-flight sale mutation.
It does not close all J03/J04: provider mutation while admitted, a second explicit
customer-specific schedule beyond the default, concurrent-editor stale revision,
full denied/loading/retry coverage, localized sale UI, and exact candidate rerun
remain. The real-provider budget stays at 2/5 attempts, 16,896 CNY millionths
reserved and 52 millionths conservative usage estimate.
