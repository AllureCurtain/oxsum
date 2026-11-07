# Closeout: exact code candidate and retained artifacts

Issues [#620](https://github.com/AllureCurtain/oxsum/issues/620) and
[#695](https://github.com/AllureCurtain/oxsum/issues/695). Candidate:
`813c788613f9f2a737562493c3b305ca1aaff5c1` (PR #694 merge, 2026-11-07).
Subsequent closeout documentation does not replace this immutable code SHA.

## Full CI

[Run 42337766660](https://github.com/AllureCurtain/oxsum/actions/runs/42337766660)
was manually dispatched while main pointed at this candidate. All 12 jobs
passed; the path filter could not skip the Rust/heavy tier. The evidence
validator independently accepted the run and retained its
[JSON verdict](artifacts/closeout-candidate/full-ci.json).

| Executed checks | Result |
| --- | --- |
| Three archived nextest shards | 650 + 594 + 536 = **1,780 passed**, six explicitly ignored across the workspace |
| Process-shared suites | **26 passed** |
| doubleentry PostgreSQL conformance | **33 passed** |
| Library / documentation tests | **675 / 29 passed**, five ignored doctests |
| fmt, strict Clippy, WASM and frontend | All passed |

The library tests overlap the nextest workspace; these are not additive
unique-test counts. Each shard filters out the other partitions, so summing
its skipped count would incorrectly report those other shards as unexecuted.
[Counts and log excerpts](artifacts/closeout-candidate/test-counts.json)
preserve that distinction. PostgreSQL 17 was available to the suites; engine
conformance and process-shared steps actually ran rather than reporting a
path-filtered green result.

Reproduce the CI-evidence check without running any publication workflow:

```bash
python3 scripts/check-candidate-ci.py \
  --sha 813c788613f9f2a737562493c3b305ca1aaff5c1 \
  --repository AllureCurtain/oxsum --output candidate-ci.json
```

## Candidate gate

[Run 42338752086](https://github.com/AllureCurtain/oxsum/actions/runs/42338752086)
completed **all ten jobs successfully**. Every job checks out the same full
SHA. The [manifest](artifacts/closeout-candidate/candidate-manifest.json)
records that SHA and the full-CI URL; the
[image smoke log](artifacts/closeout-candidate/candidate-smoke.log) reports
`SMOKE PASSED` and the exact SHA from the running image.

Downloaded [candidate-bundle artifact](https://github.com/AllureCurtain/oxsum/actions/runs/42338752086/artifacts/13560927830):
47,993,248 bytes, GitHub retention through 2027-02-05. It includes the Docker
archive, SDK tarball, frontend assets, WASM, full-CI evidence, smoke log,
[checksums](artifacts/closeout-candidate/SHA256SUMS) and notes draft. All **15
file checksums** matched locally; the downloaded ZIP also matched GitHub's
artifact digest. The Docker archive's OCI index, manifest, config and layer
digests were checked without starting another container.

| Identity | Value |
| --- | --- |
| Image platform | `linux/amd64` |
| Image OCI manifest digest | `sha256:ab431efdcfcb0089578893ad64691b577f6308e6e3a1a3e1c0f94cc33113f180` |
| Compressed Docker archive SHA-256 | `99be4081cc4e8faaea81ebcd8aed98281466a5a0430e63e8c6ef0f7fc8cbee27` |
| Downloaded bundle ZIP SHA-256 | `baee85d8b8712bf29595eaca7a7ab8220187bdc0c0edea61b78ae7ed956cc924` |

The GHCR-style image name is a local build tag; no registry push occurred.
The archive remains reviewable without publication. The
[inspection record](artifacts/closeout-candidate/artifact-inspection.json)
retains the distinctions between artifact ZIP, compressed image archive,
image manifest and config digests. Keep a local copy before artifact expiry:

```bash
gh run download 42338752086 -n candidate-bundle -D candidate-bundle
(cd candidate-bundle/artifacts && shasum -a 256 -c ../SHA256SUMS)
```

Loading that archive is optional for another isolated smoke; it is not a
live deployment or a release decision.

The candidate SDK was already downloaded and installed in a clean local
temporary project. It verifies the actual eight-unit bill exported during
[the browser journey](closeout-browser.md), rejects the all-zero expected
hash and makes zero network calls. The
[verdicts](artifacts/closeout-candidate/actual-bill-verification.json) are
retained alongside the actual proof. The workflow's mandatory consumer job
also checks its independent v1/v2 fixtures and mutations offline.

## Scope and reproducibility

The image smoke covers config, fresh migration, readiness/health/version,
owner setup/login/business context and customer invitation issuance. It
asserts the image's reported git SHA equals the requested candidate. The
mandatory separate consumer covers the same SHA's packaged verifier; the
smoke's `SMOKE_SKIP_CONSUMER=1` does not skip that mandatory job.

The browser source changes exercised incrementally have no frontend source
diff from this SHA, and the final browser server has no backend/runtime
source diff from it. The interactive journey was not replayed in full
against the candidate Docker image. Domain/HTTP tests and Node/WASM consumer
checks must not be mislabeled as browser interaction.

This evidence neither waives D09's measured performance shortfall nor proves
PSP capture/refund, all real model protocols, independent pilots or public
release. Those limits remain in [RELEASE-CHECKLIST](../RELEASE-CHECKLIST.md).
