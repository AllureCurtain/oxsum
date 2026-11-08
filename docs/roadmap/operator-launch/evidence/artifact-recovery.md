# Actual development-artifact recovery (#833)

This N17.c unit adds the reusable [artifact recovery runner](../../../artifact-recovery-rehearsal.md)
and exercised actual frozen native binaries, real PostgreSQL databases, gateway
settlements and offline cryptographic verification. Its tier is explicitly
`development-artifact-rehearsal`, not final-candidate acceptance. No public
registry, live deployment, provider or PSP was used.

## Exact artifact inputs

All three binaries were built from clean detached source checkouts on the local
macOS host, with `cargo build -p oxsum-server --bin oxsum` and build SHA equal to
the checkout's full HEAD. Each result was copied immediately and hashed before
the next build. The runner copied those original bytes again, verified hashes
and required each actual `/api/v1/version` response to match source/package/schema.
The shared target cache needed a forced source-timestamp refresh for the rollback
core/server after an old-core cache mismatch; no source content changed. These
are native debug artifacts, not release images or reproducible-build attestations.

| Role | Source | Required schema | Binary SHA-256 |
| --- | --- | --- | --- |
| Old | `45a833e2489b3e905374e9eb1b398c85e4a8fe02` | `0110_plan_refund_effects` | `d9f90e35f9c7c0caa411f2ee8db36bea539bcf588ca07772a99ae08487fe7f87` |
| Current | `8d18170f85c9648301d63659b1ae28763eaf617c` | `0111_plan_usage_corrections` | `a7c736e66650b3a5c4f4aac7e1b4d4b93d3ead087a0b791340a61a78116c6f58` |
| Compatible retained rollback | `68b3441788441d40995b1d2adefcfc43f8eb4f2e` | `0111_plan_usage_corrections` | `ef8edccacf75ef3dadf8db0946e729204c71b085df3da7db7a06a49f2d59139a` |

Rollback compatibility is deliberately limited to this fixture: source `68b3441`
already contains the same 0111 financial/runtime implementation as `8d18170`;
the intervening production changes concern provider UI and backup tooling. Both
actual binaries answered the same schema and preserved fixture data/new calls.
The old 0110 artifact was never restarted on the newer schema.

Verifier tarball was built once from `8d18170` using the actual WASM build and
`npm pack`; SHA-256
`ecc6e8d33cfecb1fad60d591376777ed939e583ff36d452a35f9135f481baaf9`.
The original tar bytes were installed with lifecycle scripts disabled and two
separate empty npm configuration files. The PostgreSQL image was
`postgres@sha256:266d3c257594e195b309ae9d1805e54c788f1d7b375f56bf6c367aac6c917e7b`
(PostgreSQL 17.11), with independent source and restore containers/volumes.

## Observed final run

Retained local evidence: `/tmp/oxsum-artifact-recovery-833-final`.
Report SHA-256: `a0dc6ef18f6bac5a24837dfcbcdcf9bd8f322014b62983007c4c668c6ac67b0e`.
Checksum index SHA-256: `eaa91c4585e92e0c0c59bced48732cc21166e6f711bd1003eb2171e7cbbdf31e`.
Both PostgreSQL containers, anonymous volumes, application processes and private
transient credentials were removed after the run. The retained dump/evidence
and binaries are private, not public support attachments.

| Stage | Observed result |
| --- | --- |
| Old runtime | Two customer accounts provisioned through owner consent/invitations; each synthetic manual receipt funded exactly 100,000,000 minor units; immutable sale terms and provider credentials established through existing APIs. Each old gateway call charged exactly 34 once with one upstream attempt. |
| Graceful drain | A real SSE request held a visible reservation. SIGTERM arrived while upstream was blocked; the process remained alive until the fixture released it. Complete SSE and clean exit, **288 ms**; upgraded reads confirmed the once-only final settlement. |
| Upgrade | Actual current binary `--migrate-only` advanced 0110 to 0111 in **1,160 ms**. Exact runtime identity, retained customer key, sealed provider credential, signing identity and both customer balances passed. A new streamed call settled exactly. |
| Backup / separate restore | Production helper exported all schemas with three organizations (owner plus two customers), two ledger schemas, two deposits and four usage rows. **484,863-byte** dump in **179 ms**, separate-container restore in **519 ms**; schema/counts matched. The restored current artifact served a new call and re-exported the old entry's proof. |
| Compatible rollback | Actual retained same-schema binary migrated idempotently and served the restored database. Original key/signing identity remained usable; another streamed call charged 34 once. Primary customer finished at **99,999,830 available**, **0 reserved**; second customer stayed at **99,999,966**. Six total upstream calls, no duplicates. |
| Offline verification | With every application stopped, the frozen SDK verified the original proof, the old entry re-exported after restore, and the new restored-call proof. Bill hash/inclusion/charge/price/sale, head signature and consistency passed. Altered bundle, expected hash, operator key and archived head were rejected; fetch was disabled and never called. |

The old entry kept content hash
`a5e8ed17a085ed5a8bf16ba5762f4517f73fe663d2402fbd0789586508909049`
when re-exported against a later restored head (size 3 originally, size 7 after
restore). This checks retained ledger evidence rather than merely reopening a
copy of the original JSON. The post-restore call had a distinct entry/proof.

Backup SHA-256:
`77299d52412c9fef0b3125a69b28904295e3d6170820fb0b92e0c6413c7bd477`.
Source physical database size: 16,324,275 bytes; target: 16,209,587 bytes.
Observed settings: `max_locks_per_transaction=64`,
`max_pred_locks_per_transaction=64`, `max_connections=100`,
`shared_buffers=134217728`. These tiny-fixture timings/settings are not production
capacity guidance, uptime, RPO or RTO guarantees.

Five runner boundary checks passed (hash freezing, environment isolation,
loopback-only targets, fatal runtime-identity mismatch and reservation
conservation refusal); JavaScript syntax, pinned formatting and workflow lint
passed. No production Rust code changed. Three actual binary builds and the
existing cryptographic consumer provide the relevant runtime verification.
The first run reached all runtime stages but npm rejected using the same
`/dev/null` config for both user and global roles. Separate empty config files
fixed that harness issue; the final complete run passed without rebuilding the
frozen artifacts.

## Remaining N17/J16 gates

N17/J16 is not closed. Repeat on the final frozen candidate image and declared
supported-old artifact; include a representative dataset, capacity-driven
settings, crash/recovery and backpressure cases beyond this graceful stream,
relevant financial policy/migration cases, and measured recovery objectives.
External witnesses, PSP/provider certification, browser journeys, live rollout
and independent operator pilots remain separate requirements. Later main commits
do not inherit this run's artifact verdict.
