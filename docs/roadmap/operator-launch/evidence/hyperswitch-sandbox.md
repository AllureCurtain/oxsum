# Pinned local Hyperswitch evidence

Observed 2026-11-08 on Docker linux/arm64. Issue #757, N06/#732 and N15/#741.
This is development fixture evidence, not final candidate or PSP certification.

## Exact compatibility set

The source commit is `cf913c5aaa226e102b257a81a3a53ac841bf45bd` (v1.129.0).
[`versions.json`](../../../../infra/hyperswitch/versions.json) records all image
manifest digests and both architecture-specific Diesel 2.3.5 checksums. The
router reports the upstream `v1.129.0-dirty-cf913c5` build string. No claim of
a pristine upstream build or native amd64 capacity qualification is made.

The task-only Compose project is `oxsum-hs-e1ffbfc6`. Its merchant is
`oxsum_fixture_70048def99e8`, profile `pro_uXAceW4QgTsOPcxC2Jk4`, and dummy
connector `mca_JgUFfiOLlL5wddPmnJkE`. These are public identities; private keys
remain in the ignored, mode-0700 runtime directory with mode-0600 files.

## Executed checks

- Source/tag/digest preparation and checksum-verified migration completed.
  Router, SDK, Control Center, PostgreSQL, Redis and local Mailpit are healthy.
- Repository formatting, strict workspace Clippy and Python compilation passed.
- Seven Python boundary checks passed: private atomic files, explicit project
  isolation, immutable pins, origin validation, admin/secret rejection,
  separate SDK/API/callback surfaces, and administrative redirect refusal.
- Repeated `prepare`, `check`, provisioning and forced container recreation
  preserve merchant/profile/connector/API-key/signing identities. The PG17 data
  mount is `/var/lib/postgresql/data`; Redis AOF and Mailpit use named volumes.
- SMTP captured local fixture messages. At least eight retained messages were
  observed after recreation; no external email was sent.
- The served `HyperLoader.js` contains both configured local SDK and API origins;
  the SDK uses the image's actual `sdkEnv`, `ENV_BACKEND_URL` and `ENV_SDK_URL`.
- The order/refund smoke used the #759 correlation-fix binary, SHA-256
  `7a328782b26baad9a9d0df58515f244195ae83d638dfd048194486f9e66c193a`.
  Baseline code failed on the actual switch refund metadata, leading to #759.
  No synthetic missing metadata was added to hide that failure.
- The passing run (`smoke-38b2549b26f7`) created order
  `0cf16c52-2e3a-4d9e-87ca-854152eea0eb`, provider payment
  `pay_QifclAXvRBDGwazWu0je`, refund `32995ee0-2832-4cc4-a1b4-1c949412aaeb`,
  and provider refund `ref_R42OQdbcQwxLX9dKGdg2`.
  With USD 1 per credit, USD 2 collected and USD 0.50 refunded, the ending
  balance was exactly 1,500,000 credit-minor units. Pending/provider-only success
  did not credit; bad signatures were refused; duplicate create/events had no
  duplicate effect; one linked applied credit note carries its ledger entry.
- The smoke stops its own listener and drops only its uniquely named scratch
  database. Private diagnostics and sanitized result remain in the task runtime.

## Remaining gates

The payment/refund calls use Hyperswitch's in-process `paypal_test` connector.
Events were **locally signed from the actual returned switch objects**. This
passes local integration, not automatic callback delivery, browser checkout,
Stripe/PayPal certification, live payment or reconciliation with a real PSP.

Free temporary Cloudflare tunnels did not connect: TLS failed with the OS
Fake-IP DNS addresses and with a real DoH-resolved edge address. All temporary
proxies/tunnels were stopped. TLS/destination policy and system DNS were not
weakened. Actual public HTTPS SDK/API/callback acceptance remains pending.
The task browser space is also unavailable; no replacement user space was used.

The current baseline selects an environment bootstrap merchant for customer
checkout. The owner-configured merchant selection/UI is separate N06 software.
Hyperswitch lost-response/retry identity is issue #760; this one-shot success
smoke does not close it. Genuine PSP credentials are absent. No real money,
model inference, paid cloud service or public release was used.
