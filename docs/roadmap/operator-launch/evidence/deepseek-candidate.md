# Bounded DeepSeek candidate-artifact evidence (#1044)

Executed 2026-11-11 12:35 Asia/Shanghai. Parent #740 / #725, N14. This is
observed real-provider evidence on the frozen release-candidate native binary.
It re-certifies the whole/SSE chat path on the candidate artifact without
re-opening the completed development scenarios.

## Exact runtime and protocol

- Source `cc16bb8fe5d6875ee2bb054e2c436cc788fc94ef`: the frozen candidate gated
  by full-tier CI [42951039071](https://github.com/AllureCurtain/oxsum/actions/runs/42951039071)
  and release-candidate bundle [42953183423](https://github.com/AllureCurtain/oxsum/actions/runs/42953183423);
  schema is `0131_invoice_corrections`, version 0.1.0. The observed runtime
  `GET /api/v1/version` reports `gitSha=cc16bb8f…`, confirming the build baked
  the candidate SHA rather than a cached worktree HEAD.
- Binary SHA-256: `ed097303dcde1f3218ada803648b3e3312f6d2236e23ea2c03bd390742d8ee31`.
- Original verifier tarball SHA-256:
  `a3ce667240f11bf209f34306973adbd83c79d487c73391486ea5571cb377325c`
  (`verify-oxsum-bill@0.1.0` from the candidate bundle).
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

## Scenario tagging

The candidate run re-certifies a different frozen binary under the shared run-wide
allowance, so its two scenarios carry the `-candidate` tag (`whole-candidate`,
`sse-candidate`). Each named scenario still claims and reserves exactly once; the
suffix does not reset the allowance. The wire protocol remains whole/SSE; the tag
distinguishes the artifact under test so the completed development `whole`/`sse`
scenarios stay closed.

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
head and consistency from a previously retained head. Each export rejected the
altered content/catalog/sale/rate/hash/key/head cases. No witness or full
period/statement bundle was present, and those checks were explicitly skipped.

## Budget and retained evidence

The shared original journal now records **4/5 attempts**, **CNY0.033792 reserved**
and **CNY0.000104 conservative observed-usage cost at peak rates**. The candidate
certification consumed attempts 3-4/5. This is not a provider invoice or an
asserted actual debit; cache/off-peak prices may be lower. The lifetime
reservations stay consumed. Both candidate scenarios are completed and cannot be
automatically rerun. No other account, payment or paid service was used.

[Official pricing](https://api-docs.deepseek.com/zh-cn/quick_start/pricing) and
[thinking parameters](https://api-docs.deepseek.com/guides/thinking_mode) were
reread before execution: peak cache-miss input $0.30/M, output $1.20/M; the
journal's CNY2/M input, CNY8/M output bound remains the conservative upper bound.
Each attempt reserved 4,096 input plus 32 output tokens before transmission, using
atomic replacement and fsync. The global CNY0.10 ceiling remains unchanged.

[Sanitized report](deepseek-candidate/report.json), [checksums](deepseek-candidate/checksums.json),
[whole verification](deepseek-candidate/whole-candidate-verification.log) and
[SSE verification](deepseek-candidate/sse-candidate-verification.log) accompany the
original export bytes. Generated local customer IDs and public operator verification
keys are fixture-only; no credentials, prompts with private data or runtime logs are
published.

Before live execution, fifteen budget/transport boundary tests passed. The
candidate artifact's own gates (full-tier CI, engine conformance, image, smoke,
consumer, WASM, frontend) are unchanged and reused from the frozen bundle.

Independent review/protected integration and N14's remaining protocol/fallback/
tooling matrix rows remain. Cancellation, missing usage, tools, real cache hits,
reasoning, other protocols and provider fallback were not exercised here. This
result does not certify those rows, real PSPs, independent pilots, browser flows
or capacity.
