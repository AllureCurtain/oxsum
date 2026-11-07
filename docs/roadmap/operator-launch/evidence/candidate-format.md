# Version-2 candidate artifact gate (#752)

Date: 2026-11-08. Base `0314cbd3d5439adbc904a327a8e16704b344a0af`.
Scope: N13.a. These are tool tests, not the final operator-launch candidate.

- Seven full-CI fixtures passed, including green jobs with skipped required
  execution steps. Manual/nightly WASM/frontend jobs now execute their builds.
- Fifteen bundle tests passed: correct bytes; wrong SHA/format; missing,
  failed/skipped gates; changed/missing/extra files; config/platform/SDK mismatch;
  wrong source/layer bytes; tar/zip traversal; symlink; incomplete full CI;
  wrong/failed/skipped workflow; forged local manifest; unavailable/expired
  artifact; rechecked remote-download/full-CI/merged-source provenance.
- actionlint 1.7.12 passed both changed workflows; Python and shell checks passed.
- The real clean consumer ran against the locally packed SDK and retained its
  exact tarball via `VERIFY_CONSUMER_OUTPUT`. Offline genuine and tamper checks
  passed. SDK `0.1.0` SRI is retained in the ignored task evidence.
- A locally built `FROM scratch` archive-format fixture (not oxsum, not a
  runnable release) was saved through Docker. Config, layers, source and
  linux/amd64 metadata validated. The containerd store returned an index ID
  different from the config digest, confirming why the two must be separated.
  The fixture image was removed afterward; its small archive is task evidence.
- Repository formatting and strict workspace Clippy passed. No Rust runtime or
  interface field changed; no database suite is needed for this tooling slice.

Local evidence: `local-notes/operator-launch/n13-consumer.log`, `n13-sdk/`,
`n13-image-format.tar`, `n13-clippy.log`. No public registry/npm/release action or
existing deployment was touched. Actual full CI/candidate workflow execution
requires integration; older candidates do not satisfy the new format. Immutable
promotion and digest deployment remain a separate N13 child.
