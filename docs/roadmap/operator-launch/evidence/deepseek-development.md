# Bounded DeepSeek development-artifact evidence (#944)

Executed 2026-11-10 08:55 Asia/Shanghai. Parent #740 / #725, N14. This is
observed real-provider evidence on a frozen native development binary. It is not
an exact release-candidate verdict or certification of unmerged application code.

## Exact runtime and protocol

- Source `1e3dc087d2999cca25ba96b3d4275c79dce228b3`: main `5285fc0b` plus
  build-only #923/#926. Production runtime code matches main; schema is
  `0119_nonplan_chargeback_corrections`, version 0.1.0.
- Binary SHA-256: `4d0f99c7b841d29d3a154f74f7097d34d0a1f1974a30b4e78a716f374c6b013a`.
- Original verifier tarball SHA-256:
  `eb191606411d24b62c1b866bea51b71ef34bf16e230003536bab469a4cda1b84`.
- POST `https://api.deepseek.com/chat/completions`, model alias `deepseek-flash`.
  The official documentation identified DeepSeek-V4.1-Flash-1017 at execution;
  the harness retains the alias and usage, not the upstream response model field,
  so the concrete model version is a documented mapping, not an observed assertion.
- Exact fixed message `Reply with exactly: OK`, `thinking: {type: disabled}`,
  max_tokens 32; the SSE request also uses `stream_options.include_usage=true`.
  No tools, history, external fallback or automatic provider retry was enabled.
- Public DNS was observed at runtime; a validated public IPv4 destination was
  pinned with original-host TLS verification, no environment proxy or redirect.
  The gateway used an explicitly approved loopback guard. This is mediated-path
  evidence, not certification of the production system resolver.

## Actual results

| Scenario | Upstream attempts | Input/output tokens | Customer charge (credit minor) | Offline verdict |
| --- | ---: | ---: | ---: | --- |
| Whole response | 1 | 9 / 1 | 10 | Passed |
| SSE with usage and DONE | 1 | 9 / 1 | 10 | Passed |

Both upstream requests returned HTTP 200. The exact customer sale was a synthetic
one-minor-per-token fixture, independently pinned in the sale proof; it is separate
from provider cost. Explicit local synthetic funding started at 100,000,000 minor.
The harness checked available balance 99,999,990 then 99,999,980 and zero reserved,
with one settled record and one upstream attempt per request. No PSP paid this wallet.

After the listener stopped and its isolated database was dropped, a clean installed
original SDK verified both exports with networking disabled: content hash,
Merkle inclusion, recomputed charge, signed price/sale bindings, operator-signed
head and consistency from a previously retained head. Each export rejected eight
altered content/catalog/sale/rate/hash/key/head cases. The additional positive case
confirmed customer sale verification without optional wholesale binding. No witness
or full period/statement bundle was present, and those checks were explicitly skipped.

## Budget and retained evidence

The shared original journal now records **2/5 attempts**, **CNY0.016896 reserved**
and **CNY0.000052 conservative observed-usage cost at peak rates**. This is not a
provider invoice or an asserted actual debit; cache/off-peak prices may be lower.
The lifetime reservations stay consumed. Both scenarios are completed and cannot
be automatically rerun. No other account, payment or paid service was used.

[Official pricing](https://api-docs.deepseek.com/zh-cn/quick_start/pricing) and
[thinking parameters](https://api-docs.deepseek.com/guides/thinking_mode) were
reread before execution: peak cache-miss input CNY2/million, output CNY8/million.
Each attempt reserved 4,096 input plus 32 output tokens before transmission, using
atomic replacement and fsync. The global CNY0.10 ceiling remains unchanged.

[Sanitized report](deepseek-development/report.json), [checksums](deepseek-development/checksums.json),
[whole verification](deepseek-development/whole-verification.log) and
[SSE verification](deepseek-development/sse-verification.log) accompany the original
export bytes. Generated local customer IDs and public operator verification keys
are fixture-only; no credentials, prompts with private data or runtime logs are
published. Large binary/SDK artifacts remain private under the primary repository's
ignored `local-notes/operator-launch/provider-certification-944` directory.

Before live execution, the same frozen artifacts passed both local synthetic
scenarios and clean offline checks. Fifteen budget/transport boundary tests passed;
locked native build, verifier WASM, SDK pack and production frontend build passed.
No production Rust changes were made; unchanged main CI and #926 checks are reused.

Independent review/protected integration and N14's exact final-candidate matrix
remain. Cancellation, missing usage, tools, real cache hits, reasoning, other
protocols and provider fallback were not exercised here. This result does not
certify those rows, real PSPs, independent pilots, browser flows or capacity.
