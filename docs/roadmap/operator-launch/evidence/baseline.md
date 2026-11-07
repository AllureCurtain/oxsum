# Execution baseline — 2026-11-08

Source: `0314cbd3d5439adbc904a327a8e16704b344a0af`, inspected with clean local main, `git fetch`, `git pull --ff-only origin main`, GitHub issue/PR/check reads and `git worktree list --porcelain`.

- #697 is closed. #724 merged at 2026-11-08 00:00:41 Asia/Shanghai. No open PR or other oxsum task worktree existed; no active oxsum build was observed. Other projects have running builds and must not be interrupted.
- [Post-merge CI 42389119891](https://github.com/AllureCurtain/oxsum/actions/runs/42389119891) succeeded: all three integration shards and the process-shared suites actually ran. Engine conformance and path-filtered frontend/WASM build steps were skipped. It does not meet the final full-candidate gate.
- [Release image 42389119833](https://github.com/AllureCurtain/oxsum/actions/runs/42389119833) succeeded. The push workflow only builds/loads/smokes; it does not publish. `release.yml` and `deploy.yml` are manual only and were not dispatched.
- Branch protection requires `fmt`, `clippy`, `test`, `wasm`, `frontend`. No required approving-review count/ruleset was configured at inspection. Recheck before merges and review each final diff; never bypass checks or use administrator merge.
- Rust 1.99.1; nextest 0.9.150; Node 24.22.1/npm 11.19.1; CI uses Node 22. Shared Cargo target remains `.worktrees/target`.
- Existing PostgreSQL: `oxsum-postgres-1`, PostgreSQL 17, healthy, loopback 5432. No Hyperswitch or mail container at baseline. Preserve this cluster and other task data; tests use isolated databases.
- Docker is Linux aarch64, 12 vCPUs and 4,093,521,920 bytes memory; host has about 69 GiB free at inspection. This is not the native Linux amd64 4-vCPU/8-GiB reference. No equivalent host has been identified; useful local diagnostics remain runnable.
- Root `.env` is ignored and mode 0600. No credential was printed, authenticated or used for paid inference. Old switch credentials are not fresh-stack evidence.

## Findings reconciled with current source

| Finding | Current inspection | Owning units |
| --- | --- | --- |
| Mixed portfolio/operator positioning and stale first run | README/product docs still carry old paths and assumptions | N01,N02 |
| Mostly read-only provider/sale/merchant business UI | Existing business APIs provide the basis; dedicated write journeys remain | N03–N06 |
| Customer admission and UUID-only staff appointment | Complete customer invitation management and add staff consent lifecycle | N07,N08 |
| Presence-based readiness and customer admin-token requirement | Current guidance needs actor/entitlement-aware state | N09 |
| Versioned daily fee recognition | Legacy period-close policy remains; preserve it when implementing daily policy | N11 |
| Embedded provisioning scopes | Existing credentials cover customersRead/meteringWrite, not a complete app lifecycle | N21 |
| Artifact promotion/deployment rebuilds/selects moving labels | Candidate gate exists; release/deploy require exact artifact consumption | N13,N20 |
| Prior acceptance and capacity | Historical candidate 813c788; D09 19.657/s and 113.421 ms p95 missed thresholds | N10,N12,N16–N18 |
| External PSP/provider/pilot gaps | #622 remains open; supplied DeepSeek key only authorizes bounded new smoke | N14,N15,N19 |

## Sources read

The current root/server AGENTS, complete TODO history, November 8 execution brief, November 7 launch plan and configuration audit were read for this baseline. The accepted scope is now tracked in CONTRACT.md. Applicable product/architecture/API/decision/design/runbook sections are re-read before each implementation. No historical acceptance is silently reused as candidate-specific evidence.

## N00 documentation validation

- `scripts/fmt-check.sh`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed (4m 04s).
- `git diff --check`: passed.
- Checked 22 task, 14 requirement and 18 journey rows against the accepted contract, actual GitHub issue IDs and relative document links.
- Reviewed the final documentation diff for scope/authority/history accuracy. No Rust source changed, so no additional database suite was required for this documentation PR.
