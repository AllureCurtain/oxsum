# Candidate image promotion development evidence (#801)

Date: 2026-11-08. Scope: N13.b; issue #739 remains open for SDK publication,
explicit digest deployment, compatible rollback and final candidate acceptance.
This is fixture transport evidence, not a published release or J14 closure.

## Implemented behavior

- `release.yml` requires explicit source SHA, candidate run/attempt and version,
  downloads the named bundle and calls `scripts/promote-candidate.py`. No build,
  implicit `latest`/SHA tag, SDK publication or SSH step exists in this path.
- The helper freezes a private snapshot, reuses version-2 byte and online GitHub
  provenance validation, then copies original config/layer member bytes through
  the Distribution API. Only registry transport metadata is generated.
- Immutable pull verification binds manifest/index, actual config/platform/source,
  raw blob sizes/hashes and decompressed rootfs digests. The new independent
  receipt records distinct archive/config/registry identities.
- Existing versions with different config/layer bytes are refused. Same bytes
  reverify idempotently. Workflow jobs serialize; other writers require operator
  exclusion because Distribution lacks atomic tag compare-and-swap.
- GHCR token challenges require the exact realm and selected repository scope.
  Upload locations remain same-origin. Allowlisted HTTPS GitHub blob redirects
  receive no Authorization header. Registry errors never masquerade as absence.
- The only `deploy.yml` change is `actions: read` on the reusable caller, required
  for candidate provenance downloads. Missing selectors cause that legacy caller
  to fail before publication/SSH; N13.c must replace the combined path.

## Executed validation

| Check | Result |
| --- | --- |
| `python3 scripts/test-candidate-ci.py` | 7 passed |
| `python3 scripts/test-candidate-bundle.py` | 15 passed |
| `python3 scripts/test-candidate-promotion.py` | 17 passed |
| `scripts/fmt-check.sh` | passed |
| actionlint 1.7.12 on release/deploy/ci workflows | passed |
| `git diff --check` | passed |
| Real isolated registry upload and immutable download | passed |
| Same-version repeated publication | `already-present`, same digest |
| Different candidate at occupied version | refused before tag overwrite |
| Original gzip-compressed layer bytes | preserved; same config, distinct registry digest |
| Registry lifecycle | task-owned random container/loopback port, removed at exit |

No Rust code or schema changed and no local Cargo build ran for this unit. PR CI
remains its own gate. The promotion tests cover failed provenance, skipped gates,
wrong source/run/attempt/repository, changed files, original/frozen snapshot
replacement, wrong config/platform/rootfs, index ambiguity/platform mismatch,
raw blob digest/size failure, registry absence versus auth/network error, tag
conflicts/races, receipt overwrite/location, target parsing, token realm/scope,
upload location and credential stripping across approved blob redirects.

The real registry was inspected locally before downloading the pinned dependency:

```
registry:2.8.3@sha256:a3d8aaa63ed8681a604f1dea0aa03f100d5895b6a58ace528858a7b332415373
```

It ran with ephemeral storage and a random `127.0.0.1` port. No existing container,
root `.env`, database or public registry was changed. A tiny linux/amd64 fixture
archive with one real tar layer was used without executing the image. The final
recorded uncompressed fixture identities were:

```
archive SHA-256: db1da2798b2ba1fac93e2ee3ac197cd46fd3f9aedf085d4893cfa8ffafe4ae23
config ID:      sha256:6adb0f0a494a8334541802b1a9d9a377ced8c16f5be1e36c788995c498f35eb0
registry digest: sha256:4f104610202557f73cc9143ccc34c8f910fad8d215b92e8271d0fabe62623014
platform digest: sha256:4f104610202557f73cc9143ccc34c8f910fad8d215b92e8271d0fabe62623014
```

The private local output was `/tmp/oxsum-image-promotion-rehearsal-801-e`; it
contains fixture bundles and `published.json`, `repeated.json`, `compressed.json`
and `rehearsal.json`. All receipts are `local-fixture`; synthetic source/CI values
are not presented as successful GitHub candidate evidence. Fixture archive gzip
metadata can change its outer checksum between runs without changing image
config/layer identity; the retained receipt names the actual run's bytes.

## Remaining acceptance

Run the production helper against a genuinely successful, unexpired version-2
candidate only within explicit publication authority. GHCR upload/auth integration
has not been exercised here. The helper supports GHCR's documented token and
blob-redirect hosts and fails closed on unexpected responses. No public push,
release workflow dispatch, SDK publish, live restart, digest deployment or rollback
occurred. Finish N13.c/N13.d and rerun the final integrated candidate gates.
