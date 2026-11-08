# Candidate bundle format, version 2 (#752)

A candidate bundle is local evidence, not permission to publish. Its source must
be a merged full 40-character SHA. The archived image, image configuration ID
and future registry manifest digest are separate identities.

`candidate-manifest.json` contains these required fields:

- `formatVersion`: integer `2`.
- `sourceSha`, `repository`: exact source and `owner/name`.
- `workflow`: candidate workflow `runId` (decimal string), `attempt` (positive
  integer), `workflowSha` (40-character workflow revision), and canonical GitHub
  `url`. The workflow revision can differ from the selected source SHA.
- `gatedAt`: UTC RFC3339 timestamp.
- `gates`: `fmt`, `clippy`, `libDocTests`, `wasm`, `frontend`, `consumer`,
  `examples`, `image`, `smoke`, each exactly `passed`.
- `image`: `archive` (relative file), `configId` (`sha256:<64 hex>`), `platform`
  (`linux/amd64` for this release), `sourceSha`. The config ID is the SHA-256
  of the Docker archive's config bytes; its OCI revision label matches source.
  It is never called a registry digest.
- `sdk`: `tarball` (relative file), `name` (`verify-oxsum-bill`), `version` and
  `integrity` (npm SHA-512 SRI of those exact compressed bytes). This is the
  same file installed in the clean consumer; no second pack is permitted.
- `evidence`: relative `fullCi`, `smokeLog`, `consumerLog` files. New candidate
  runs also retain `runtimeVersion`, the actual readiness-checked
  `/api/v1/version` JSON (`data.gitSha`, `data.version`, `data.schemaVersion`).
  Version-2 bundles without this optional field remain valid for promotion, but
  accepted digest deployment requires it and refuses historical missing evidence. Full CI names
  an executed successful main run on sourceSha, including all integration
  shards, engine/process-shared suites and real WASM/frontend builds.
- `artifacts`: a map from every retained relative artifact/notes path to
  `{sha256, size}`. Paths are POSIX, never absolute, symlinked or containing
  `..`. No unlisted file may masquerade as a candidate artifact.

`SHA256SUMS` covers the manifest plus every artifact, but not itself. Offline
validation checks the file set, sizes/hashes, archive paths, actual image config
and source/platform, SDK metadata/WASM, mandatory gates and embedded CI identity.
It cannot authenticate who produced a self-consistent bundle.

Online validation additionally reads GitHub: the exact candidate workflow run
and attempt must be completed successfully in this repository, its workflow
revision must match, required jobs must have succeeded, and its `candidate-bundle`
artifact must still exist and be unexpired. It downloads that artifact into a
fresh temporary directory and compares its manifest/checksums to the supplied
bundle, then rechecks the exact referenced full-CI run and its executed steps.
Both source and workflow revisions must be ancestors of current main. A same-name
workflow or a local JSON assertion cannot replace that provenance.

Older version-1 bundles must be regenerated; they are not silently promoted.
Every rebuild is a new artifact requiring validation. Promotion consumes these
bytes and records the separate registry digest after upload. Image promotion is implemented by `scripts/promote-candidate.py` (#801).
Digest deployment and readiness/source/schema checks are implemented by N13.c
(#810); execution still requires its action-specific authority.

Docker's containerd image store can report an OCI index digest as `.Id`, while
classic Docker reports the config ID there. The manifest deliberately hashes the
archive's configuration bytes and its rootfs layer digests. A promotion runner
must inspect the registry manifest/config relationship rather than compare a
containerd index ID to `configId` or label either one a tar checksum.


## Image promotion receipt, version 1 (#801)

A receipt is independent of the version-2 bundle: promotion never edits its
manifest or checksum list. `promotion-receipt.json` is written only after online
provenance and registry pull checks succeed (except the loopback-only fixture
mode described below). It contains:

- `formatVersion`: integer `1`; `evidenceTier`: `verified-candidate` or
  `local-fixture`. Fixture receipts cannot attest GitHub provenance or a release.
- `sourceSha`, `repository`, `workflow`: the candidate's exact source and
  run/attempt/workflow revision/URL, checked against explicit caller selectors.
- `candidateManifestSha256`, `archiveSha256`: hashes of original candidate files.
- `configId`, `platform`: original configuration bytes and `linux/amd64`.
- `destination`: explicit registry/repository/version tag.
- `registryDigest`: top-level registry manifest or index digest.
- `platformManifestDigest`: the selected linux/amd64 manifest digest; equal to
  `registryDigest` for the single-platform manifest this helper uploads.
- `immutableReference`: `registry/repository@sha256:<digest>` using the top-level
  digest, never the configuration ID or archive checksum.
- `result`: `published` or `already-present`; `promotedAt`: UTC RFC3339 time.

The helper generates a registry transport manifest referring to the archive's
unchanged config and ordered layer bytes. It checks existing config/layer
hashes and sizes before accepting an idempotent version. Pull verification
checks every registry blob and each uncompressed rootfs digest; a tar checksum
cannot establish that identity. Registry indexes must select exactly one
linux/amd64 image; only separately labeled unknown/unknown attestations may
coexist. No additional serving platform is implicitly certified.

Version tags require serialization across every writer; Distribution has no
atomic tag compare-and-swap. The workflow serializes its own jobs, rechecks
conflicts immediately before tagging and checks the final tag after pull.
Direct or external writers must observe the same operator exclusion. Fixture
mode permits only `127.0.0.1:<port>` and does not contact GitHub or certify the
fixture's asserted CI. Production always uses online provenance and GHCR.


## Deployment plan and retained state (#810)

`deployment_plan.py` freezes and validates the candidate online, requires its
observed `runtimeVersion` artifact and verifies the selected fixed GHCR digest
against original config/layer bytes. Registry authority is read-only. The plan
records version 1, `verified-candidate`, exact source/repository/workflow,
manifest/archive hashes, image reference/config/platform/top-level/platform
manifest digests, observed runtime fields and preparation time.

The plan is not signed. Only accept it through the same trusted authenticated
channel that ran its preparation; schema validation alone cannot authenticate a
locally edited record. Deploy's workflow prepares and transfers it in one job.
Runtime execution checks the pulled saved image and exact readiness/version,
then writes a successful receipt with the full plan, actual runtime, target
container/port, operator-environment hash, prior digest and completion time.
Private mode-0700 state retains active/previous receipts and a host process lock.

A same-schema rollback requires explicit compatibility confirmation, both
successful receipts, exact previous digest/source selection, unchanged current/
previous operator environment and exact current runtime. Otherwise restore to a
separate database. A local HTTP/Docker state fixture proves only orchestration
branches; it cannot provide `verified-candidate` acceptance for a real runtime.
