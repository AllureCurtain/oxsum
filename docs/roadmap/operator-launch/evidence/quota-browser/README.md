# Browser quota exhaustion and explicit overage

Issue #986, N10/J10 development acceptance. Actual customer Chat, plan and owner
customer views on backend/frontend `3b5b55be4b54cf4bf9ba35c87d6119bd4b108ff7`;
exact binary identity is retained in `runtime.json`. Local model/payment fixtures
only; the persistent real-provider budget stays 2/5 attempts.

The paid anniversary period originally had 80 of 100 credit-minor units consumed,
with cancellation scheduled at its future boundary. Its sold version explicitly
allows wallet overage. Browser calls settled as follows (1 credit = 1,000,000):

| Stage | Quota consumed | Wallet available | New charge | Quota / wallet split |
| --- | ---: | ---: | ---: | ---: |
| Before | 80 | 999956 | — | — |
| Mixed call `bfe3eef5` | 100 | 999936 | 40 | 20 / 20 |
| Fully exhausted call `7f78a8fd` | 100 | 999896 | 40 | 0 / 40 |

Both requests have one upstream attempt and no remaining holds. Customer and
staff reads agree on the original period's 100-unit consumed value. Chinese and
English browser screenshots show that ceiling and the cancellation flag. The
original proof's subscription funding describes the 20-unit leg; no quota was
turned into wallet cash.

A separately invited personal customer received a 100-unit complimentary plan
with overage disabled. An actual browser request needing a 240-unit reservation
returned 402 with zero charge, zero upstream attempts, unchanged quota and no
hold. The customer then created a local USD0.01 wallet order
`5c766b63-38e6-43c5-a193-0e703ea78288`; only an authenticated local payment event
funded 40,000 credit-minor units (bad signature 401, valid/replay 200). Another
actual browser call still refused under the sold no-overage policy. The funded
wallet stayed exactly 40,000, quota stayed zero and holds stayed empty. This
checks reservation refusal even when wallet funding could cover it; the bounded
plan's quota was not consumed to exhaustion. Screenshots include 390px mobile.

The new account was enrolled through owner invitation and recipient consent.
The setup-only default key was revoked; Chat uses its own browser-minted key.
Manual complimentary UUID entry exposed issue #988, repaired separately.

## Offline exported proof check

The current-main verifier was built to WASM, packed once and installed in a clean
external consumer. Original raw proof bytes and hashes retained through the bills
endpoint were supplied to `verifyBill`, with separate `verifyPriceHead` and
`verifySaleHead` verdicts. Both actual entries passed; wrong hashes and altered
charges failed. `fetch` was disabled and the counter remained zero. The tarball's
SHA-256 and source are in `runtime.json`; original verdicts are retained.
An older November 9 temporary verifier rejected these inputs, so it was not used
as current evidence. Hashes and operator keys fetched from the same local server
provide consistency checks, not independent external trust or supplier truth.

All published screenshots were visually inspected; original JSON, integer
invariants, credential patterns and SHA-256 inventory were checked. This evidence
does not close actual renewal/change/expiry, controlled monthly postpaid, PSP,
independent pilots or final candidate gates. No release was published.
