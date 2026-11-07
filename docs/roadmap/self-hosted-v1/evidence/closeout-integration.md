# Archived integration checks — closeout (#684)

The previous candidate `930794c` was not fully validated: CI run
[42240404235](https://github.com/AllureCurtain/oxsum/actions/runs/42240404235)
failed in every integration shard. The candidate workflow did not inspect
that result. This record supersedes the unqualified full-CI claim for that SHA.

## Corrections

- Server subprocess tests resolve nextest's runtime executable path before
  Cargo's fallback. The archive extracts to a new directory; the build-time
  absolute path is not portable. Both install/upgrade and ownership-preview
  suites use the helper.
- The explicit migration inventory includes all three tables introduced by
  `0096_reconciliation.sql`. Assertions remain exact; no test is skipped or
  weakened.

## Validation (2026-11-07)

Environment: macOS arm64, PostgreSQL 17 on the local Docker fixture, isolated
per-test scratch databases. No provider or merchant credentials.

- `cargo nextest archive -p oxsum-core --test identity -p oxsum-server
  --test install_ops --test ownership_preview`, then `cargo nextest run
  --archive-file ... --no-fail-fast`: **19 passed, zero skipped**. The original
  `target/debug/oxsum` was temporarily moved away during the run and restored
  on exit, so success requires the extracted executable.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo nextest run --workspace --no-fail-fast -E 'not binary(generative)
  and not binary(seed) and not binary(jobs)'`: **1,779 passed, five skipped**
  by the selection/ignore policy. The excluded process-shared binaries remain
  part of full CI, which must pass before candidate acceptance.
- `cargo fmt --all --check` and `git diff --check`: passed.

Local logs: `/tmp/oxsum-closeout/archive-test.log`, `clippy.log`,
`workspace-test.log`. Final integrated full-CI and candidate URLs are recorded
in the closeout acceptance evidence once those runs finish.
