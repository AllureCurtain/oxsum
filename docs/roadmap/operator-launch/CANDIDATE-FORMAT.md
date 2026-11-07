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
- `evidence`: relative `fullCi`, `smokeLog`, `consumerLog` files. Full CI names
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
bytes and records the separate registry digest after upload. Deployment uses
that digest and checks readiness plus the source SHA; those operations are a
separate N13 child and require their action-specific authority.

Docker's containerd image store can report an OCI index digest as `.Id`, while
classic Docker reports the config ID there. The manifest deliberately hashes the
archive's configuration bytes and its rootfs layer digests. A promotion runner
must inspect the registry manifest/config relationship rather than compare a
containerd index ID to `configId` or label either one a tar checksum.
