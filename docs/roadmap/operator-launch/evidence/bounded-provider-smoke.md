# Bounded provider harness evidence — #869

Parent: #740 / N14. This checkpoint contains no real inference or payment.

- Fifteen local boundary checks passed: process-shared scenario ownership,
  pre-existing five-attempt exhaustion, durable unknown outcomes, observed usage
  without allowance restoration, usage-bound halt/export, stale/corrupt/missing
  journal refusal, invalid token/path/body refusal, fixed public-IP policy,
  private credential parsing, and failed replace or failed directory fsync
  preventing network transmission. The latter retains the replaced reservation
  and refuses a later resend.
- Complete local whole/SSE artifact rehearsal passed with the existing frozen
  development source `8d18170f85c9648301d63659b1ae28763eaf617c`, binary SHA-256
  `a7c736e66650b3a5c4f4aac7e1b4d4b93d3ead087a0b791340a61a78116c6f58`, schema
  `0111_plan_usage_corrections`, and verifier package SHA-256
  `ecc6e8d33cfecb1fad60d591376777ed939e583ff36d452a35f9135f481baaf9`.
  Each scenario settled exactly 34 minor units; the two-call available balance
  was 99,999,932 with zero reserved. Both independent offline checks passed after
  server shutdown/database removal, including four altered-input rejections each.
  Local retained directory: `/tmp/oxsum-869-fixture-final`. This predates the
  current source and proves harness behavior only, not the final candidate.
- Official [CNY pricing](https://api-docs.deepseek.com/zh-cn/quick_start/pricing)
  and [thinking parameters](https://api-docs.deepseek.com/guides/thinking_mode)
  were read on 2026-11-09. Current documented model: `deepseek-flash`,
  DeepSeek-V4.1-Flash-1017. Peak cache-miss input/output are CNY 2/8 per million
  tokens. Thinking is enabled by default and must be explicitly disabled.
- The host's system DNS returned `198.18.0.237` (Fake-IP). An authenticated public
  DNS observation returned a CloudFront answer `3.173.21.63`. An unauthenticated,
  public-address-pinned, proxy-free HTTPS HEAD with hostname/certificate checks
  returned HTTP 401 at 2026-11-08 23:15:37 UTC. This proves that direct public TLS
  is reachable; it does not prove an inference or override gateway destination
  policy. The guard re-resolves at execution instead of embedding this address.

Independent finance source review passed after tightening per-scenario attempt
ownership, read-only halted-journal export, frozen SDK hash verification and
the post-replacement directory-fsync failure check. The final local artifact
rehearsal passed again after these changes.

The real shared journal remains **0/5 attempts, CNY 0 reserved/observed**. Review,
PR integration, the authorized live whole/SSE rows and final candidate acceptance
remain. [Runbook](../../../provider-smoke.md) explains the mediated-path scope,
immutable resume behavior and exact artifact inputs.
