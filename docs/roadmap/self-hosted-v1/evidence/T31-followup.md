# T31 follow-up — real-provider certification + Airwallex credential finding (2026-11-06)

Issue: [#622](https://github.com/AllureCurtain/oxsum/issues/622). Companion to
`T31-partial.md`: after the local proxy switched from fake-IP (TUN) mode to
system-proxy mode, provider hostnames resolved to their real public IPs, so
the destination policy that correctly refused `198.18.0.0/15` answers now
admits them. Two items that were environment-blocked were re-run against a
fresh `oxsum_live` database seeded from `841a74d` (pre-#679 main) on
`127.0.0.1:3020`.

## 1. Real-provider call through the gateway — verified end to end

This is the item the earlier run listed as "needs provider credentials +
paid-call authorization". A Deepseek API key was provided for certification;
quota use was kept to one minimal call.

| Step | Result |
| --- | --- |
| `POST /api/v1/admin/channels` `{"name":"deepseek","baseUrl":"https://api.deepseek.com","apiKey":"sk-…80d6","protocol":"openai","destinationPolicy":{"network":"public"}}` | **200** — channel `d4ae9898`, credential sealed (`apiKeyLast4` only), `settings.destinationPolicy.network=public` |
| `POST /api/v1/admin/channels/deepseek/prices` `deepseek-chat` | **200** — price version 1 (`input 2 000`, `output 8 000`, `maxOutputTokens 8 192`, upstream rates recorded) |
| `PUT /api/v1/admin/organizations/{acme}/groups/ds` + `PATCH /api/v1/org/keys/{seedKey}` `groupId` | **200** — the seed consumption key serves only through `deepseek` |
| `POST /v1/chat/completions` `{"model":"deepseek-chat","messages":[{"role":"user","content":"Say OK"}],"max_tokens":4}` | **200** — upstream answered `"OK"`; `x-oxsum-request-id 940db07e`, `x-oxsum-freeze-minor 1`, `x-oxsum-charged-minor 1` |
| Ledger | hold entry index 153 (`freeze 1`) → settle entry index 154 (`charged 1`, `usage {input 6, output 1}`, `priceVersion 1`, `upstreamAttempts 1`, `channel "deepseek"`) — `v14` entry binds the price row |
| `GET /api/v1/entries/3ce766d0…/proof` + `log/head` + `log/key` + `log/consistency?from=152` | exported into `verify-consumer` fixtures; `node verify.mjs` → **all PASS** (bill hash/inclusion/charge/price/sale, head operator signature, consistency extension) and all four rejection checks rejected |

The real upstream usage was `prompt_tokens 6, completion_tokens 1` — the
smallest billable turn Deepseek serves. No stream, no retry, no fallback:
one attempt, one settle.

**This closes the real-provider certification item** for the test tier: a
paid call ran through the gateway's own hold/settle path against the live
provider, and the exported evidence verifies offline exactly as a customer's
browser would check it.

## 2. Airwallex outbound — credential finding, not a network block

With DNS now returning `api-demo.airwallex.com → 114.117.133.34` (a public
IP the `public` policy admits), `POST /api/v1/topups/airwallex` was retried
letting oxsum create the PaymentIntent itself. The result was
`SERVICE_UNAVAILABLE` from oxsum, and a direct probe of the provider's auth
endpoint explains it:

```
POST https://api-demo.airwallex.com/api/v1/authentication/login
x-client-id: <.env OXSUM_AIRWALLEX_CLIENT_ID>
x-api-key:    <.env OXSUM_AIRWALLEX_API_KEY>

→ HTTP/2 403 {"code":"unauthorized","message":"Forbidden"}
```

The same 403 returns with no headers, with `Accept`/`User-Agent` set, with
the system proxy bypassed (`--noproxy '*'`), and pinned directly to the
resolved IP (`--resolve …:443:114.117.133.34`). The network path is clean —
**the stored demo credential is rejected by Airwallex itself**, most likely
rotated or deprovisioned since it was issued. oxsum's outbound hop is doing
the right request shape; the provider will not issue the bearer token the
intent create needs.

This stays external-pending, but for a new reason: it needs a *working*
Airwallex demo/test credential, not a network change. The receive→credit
→reverse leg remains verified from `T31-partial.md` §4 (in-repo webhook path,
HMAC-verified, ledger-conserving), and the create-intent code path is the
same one the live rail uses — only the upstream auth answer differs.

## 3. Updated external-pending list

| Item | State |
| --- | --- |
| Real-provider certified call | **Done (test tier)** — this document §1 |
| Merchant sandbox payment create | Blocked on a *valid* Airwallex demo credential (403 on login); receive/refund legs verified in `T31-partial.md` §4 |
| Low-value live payment/refund | Still needs authorized real-money credentials |
| Two independent pilots | Still none authorized — internal only |
| G5 publication decision | Still pending per chosen SHA |
| npm publication | Still no npm account; packed tarball verified |

## Environment note (updated)

Runs in this document used the machine's system-proxy mode with fake-IP DNS
disabled. `api.deepseek.com` resolved to real public addresses
(`111.8.x.x`, `120.226.x.x`), `api-demo.airwallex.com` to `114.117.133.34`,
`api.stripe.com` to real `198.x.x.x` — the `ProviderNetwork::Public`
destination policy admitted all of them, which is the behaviour the earlier
runs could not reach. GitHub `git push` needed the local HTTP proxy pinned
(`http://127.0.0.1:7897`) because git does not read `scutil`; `gh` and
`curl` pick the system proxy up themselves.

## Gate outcome (resolved after this document's first commit)

The candidate-gate story begun in `T31-partial.md` §1 closed green: dispatch
`42240437323` on `930794c` ran all ten jobs to `success` and produced the
`candidate-bundle` artifact — the first end-to-end clean run on a real SHA.
Three dispatches each surfaced one latent defect and each was fixed in its
own PR before the next attempt: `wasm-bindgen` release-asset 404 (#677,
crates.io install), missing `sccache-action` before the compile step plus a
missing `build-verify-wasm.sh` ahead of `npm pack` (#679), and a
`${{ needs.resolve.outputs.sha }}` typo the earlier failures had hidden
(#681). The gate now does what T29 designed it to do on an arbitrary main
commit.
