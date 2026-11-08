# Release candidate and promotion

How a commit becomes a release candidate, what the gate proves, and how an
operator promotes a candidate — the T29 gate layer between "main is green"
and "a version is published". Publishing (`release.yml`) and deploying
(`deploy.yml`) stay manual-only; this runbook's gate never touches either.

## Gate order

| Gate | What runs | Where | On what |
| --- | --- | --- | --- |
| 0 | Full CI heavy tier — all three nextest shards, process-shared suites and doubleentry Postgres conformance, executed successfully | `ci.yml` on main; dispatch a full run when a push skips anything | the exact candidate SHA |
| 1 | `release-candidate.yml` — fmt, clippy, lib/doc tests, wasm build, frontend build, verify-consumer, candidate image build, manifest + checksums + notes draft | manual `gh workflow run release-candidate.yml -f sha=<sha>` | the exact promoted SHA |
| 2 | `scripts/release-candidate-smoke.sh` — live smoke, including an exact version SHA assertion | candidate image job, automatically; repeatable locally | the image built in gate 1 |
| 3 | `release.yml` — validate online provenance, upload the original image config/layer bytes, retain immutable digest receipt | manual dispatch with candidate run/attempt/source and explicit version | the saved candidate archive |
| 3b | `release-sdk.yml` — publish the original candidate SDK tarball and verify downloaded SRI/bytes | separate authorized manual dispatch with package version/tag | the same `.tgz` installed by candidate consumer |
| 4 | `deploy.yml` — verify a published candidate digest, migrate/serve it and check readiness/version | separate authorized manual dispatch | immutable digest plus candidate run/attempt/source |

Gate 1 fails closed unless `check-candidate-ci.py` finds a successful
completed `ci.yml` run on exactly the candidate SHA and main branch. Every
shard and the process-shared and engine-conformance steps must have executed
successfully. A green path-filtered run or a run that skipped conformance is
insufficient. Run `gh workflow run ci.yml --ref main` while main points to the
chosen candidate, wait for it to finish, then dispatch the candidate gate.
The validated CI URL, attempt and job verdicts are retained in
`candidate-ci.json` and linked from the candidate manifest. Fixture tests in
`test-candidate-ci.py` cover failed, missing, skipped and wrong-SHA evidence.
Each required job must also carry its named successful execution steps, including
WASM/frontend builds and library/documentation tests; job-level success cannot
mask a skipped build. Manual/nightly CI now executes both Docker stages.

## What gate 1 produces

Run it against an exact commit:

```bash
gh workflow run release-candidate.yml -f sha=<40-char commit>
```

The workflow first resolves the SHA: it must be a commit *and an ancestor
of main* — a candidate is a merged commit, not a side branch. Every job
then checks out that SHA explicitly, so the verdict can never drift to
whatever the branch pointed at when the run started.

Per step:

- **fmt / clippy / test** — the same fast tier a PR gates on
  (`scripts/fmt-check.sh`, `cargo clippy --workspace --all-targets`,
  (`scripts/fmt-check.sh` with the pinned nightly, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --lib` + `--doc`).
- **wasm** — `scripts/build-verify-wasm.sh` compiles `oxsum_verify` for
  wasm32 with the wasm-bindgen CLI pinned to the lockfile's version; the
  bundle uploads as the `verify-wasm` artifact.
- **frontend** — `npm ci`, `npm run codegen` (the typed client regenerated
  from the SHA's own openapi.yaml), `npm run build`; `web/dist` uploads as
  `web-dist`.
- **verify-consumer** — `scripts/check-verify-consumer.sh` packs the
  `verify-oxsum-bill` tarball, installs it into a clean project outside the
  repo, verifies the genuine minted bundle and rejects the tampered,
  dropped and wrong-org fixtures — the A26/A30 evidence check, offline.
  The exact tarball it installed and verified is copied via `VERIFY_CONSUMER_OUTPUT`
  and uploads as `verify-oxsum-bill-tarball`; it is never packed again. The
  consumer log is retained as evidence.
- **examples** — `scripts/verify-examples.sh` boots a scratch deployment
  (postgres service + the server built at the promoted SHA) and runs every
  shipped developer example end to end — the A27 fresh-install half.
- **image** — the full Dockerfile at the promoted SHA with
  `GIT_SHA=<sha>` baked in (so `/api/v1/version` inside the image answers
  the gated commit). Loaded into the runner's daemon and started through the throwaway Compose smoke. The smoke checks its
  reported SHA, migration, readiness, business setup and customer invitation.
  **Never pushed**: no registry login exists in this workflow. The tested image
  is exported as `candidate-image.tar.gz`; `docker load` restores the exact
  bytes for repeat validation. `candidate-smoke.log` records the executed run;
  `candidate-runtime-version.json` retains the actual `/api/v1/version` response
  after readiness and source assertions. Its hashed bytes bind package/source/
  schema identity required by digest deployment.
- **manifest** — version 2 [`candidate-manifest.json`](roadmap/operator-launch/CANDIDATE-FORMAT.md)
  records source and workflow revisions, run/attempt, required gates, image
  config ID/platform/source label and SDK name/version/SHA-512 integrity.
  Artifact sizes/SHA-256 hashes cover every retained input; `SHA256SUMS` also
  binds the manifest. Image config ID, archive SHA-256 and a future registry
  manifest digest are explicitly different. The bundle uploads with 90-day retention.

Validate downloaded bytes before any promotion:

```bash
python3 scripts/candidate_bundle.py validate /path/to/candidate-bundle --sha <40-char-sha>
python3 scripts/candidate_bundle.py validate /path/to/candidate-bundle --sha <40-char-sha> --online
```

The offline check rejects mismatched files, source, platform, image layers,
package metadata and incomplete gates. It is not provenance authentication.
The online check requires authenticated `gh`, rechecks the named successful
candidate/full-CI attempts, requires an unexpired `candidate-bundle` artifact,
downloads it and compares its validated manifest with the local one. Missing,
expired, unavailable or wrong-run artifacts fail closed. No publishing occurs.
Version-1 manifests require a new candidate run.

## What gate 2 proves

`scripts/release-candidate-smoke.sh` runs the image on a throwaway compose
project — its own postgres volume, generated secrets, full teardown. The
profile and env file live in a temporary directory; `deploy/.env` is never
modified. The image job runs this gate with `SMOKE_BUILD=0`; the mandatory
verify-consumer job covers the SDK on the same SHA. To repeat it locally after
loading the candidate-image artifact:

```bash
OXSUM_IMAGE_REF=ghcr.io/allurecurtain/oxsum:sha-<sha> SMOKE_EXPECTED_SHA=<sha> SMOKE_BUILD=0 scripts/release-candidate-smoke.sh
```

It asserts, in order: `--check-config` parses the env; `--migrate-only`
converges the schema; `/readyz` gates green only once the schema is
current; `/api/v1/version` reports the exact `SMOKE_EXPECTED_SHA`; the T03 setup flow works end
to end (operator-token setup invitation → owner register → login →
explicit business-context switch → `GET /api/v1/business`); the staff
customer-invitation write answers; and (unless `SMOKE_SKIP_CONSUMER=1`)
the packed SDK verifies genuine and rejects tampered evidence. CI sets
`SMOKE_SKIP_CONSUMER=1` only because its separate mandatory consumer job
performs that check; the manifest depends on both jobs. Any step's
failure aborts the run loudly; success prints the image, version and
per-surface verdicts.

## A30 mapping

| A30 requirement | Where it is covered |
| --- | --- |
| Same-SHA image passes full gates | gates 0+1: heavy tier on main, fast tier + wasm + frontend + image in `release-candidate.yml` |
| Startup/version/readiness | gate 2 steps 1–3 |
| Customer + business surfaces | gate 2 steps 4–5 |
| Browser verifier positive + negative | Interactive built-UI evidence is recorded separately in `docs/roadmap/self-hosted-v1/evidence/closeout-browser.md`; the consumer jobs run the same WASM verifier under Node, not a browser |
| Clean consumer of the packed artifact | `verify-oxsum-bill-tarball` — the exact `.tgz` the consumer installed |
| Checksums/provenance/notes | `candidate-bundle`: `SHA256SUMS`, `candidate-manifest.json`, `RELEASE-NOTES-DRAFT.md` |
| No publication claim | nothing in the gate pushes; the manifest is evidence, not a release |

## Promoting a candidate

Only after gates 0–2 are green on the same SHA and the release checklist's
external/limitation decisions are satisfied:

1. Review `candidate-bundle` from the gate-1 run — the manifest names the
   exact commit every gate ran against.
2. Draft the release notes from `RELEASE-NOTES-DRAFT.md`.
3. After explicit publication authorization, dispatch the current reviewed promotion
   workflow with the exact candidate selectors and a new version:

   ```bash
   gh workflow run release.yml --ref main \
     -f source_sha=<40-character-source-sha> \
     -f candidate_run=<successful-run-id> -f candidate_attempt=<attempt> \
     -f image_tag=v<x.y.z>
   ```

   The workflow's checkout supplies the promotion tooling; it never selects or
   rebuilds application source. The downloaded candidate's own manifest, image,
   successful run/attempt and full-CI evidence determine what may be published.
   It creates only the explicit version tag, with no implicit `latest` or SHA tag.
4. Retain `image-promotion-receipt` alongside the original candidate bundle. It
   names the archive hash, image config ID, platform and immutable registry
   reference. Use that digest as the separate deployment input.

The `deploy.yml` workflow verifies the explicit published digest and requires
candidate runtime evidence before its SSH stage. It never calls Release or
builds an image. See [the deployment and rollback runbook](self-hosted-upgrade.md)
for its prerequisites, trusted plan, readiness/version checks and compatibility
rules. SDK publication uses the separate `release-sdk.yml` path described below;
final actual-candidate publication/deployment/rollback acceptance remains open; tooling alone does not close J14.

## Image promotion checks and receipt

`scripts/promote-candidate.py` requires Python 3.10+, authenticated `gh`, and
`OXSUM_REGISTRY_USERNAME`/`OXSUM_REGISTRY_TOKEN` supplied by the operator's secret
store (the workflow uses its ephemeral GitHub token). It does not load `.env`.
The account needs permission to read candidate Actions artifacts and write the
selected GHCR package. The workflow destination is fixed to
`ghcr.io/allurecurtain/oxsum`; direct tooling accepts an explicitly named GHCR
repository and `vX.Y.Z[-suffix]` version. Repository operators must authorize the
specific destination and version before invoking it.

```bash
python3 scripts/promote-candidate.py /path/to/candidate-bundle \
  --sha <40-character-source-sha> --repository AllureCurtain/oxsum \
  --run <successful-run-id> --attempt <attempt> \
  --destination ghcr.io/allurecurtain/oxsum:v<x.y.z> \
  --receipt /path/outside-candidate/promotion-receipt.json
```

The helper copies the bundle to a private temporary snapshot and completes the
version-2 offline and online checks before any registry write. It uploads the
archive's original configuration and layer member bytes through the Distribution
API. Docker archives contain no authoritative registry manifest, so the helper
creates only the transport manifest that refers to those content-addressed
bytes. It never rebuilds or recompresses layers. It then downloads by immutable
digest, checks the configuration/platform/source, every blob size/hash, and each
decompressed rootfs digest. A recognized missing manifest permits publication;
authentication, network and unrecognized registry errors fail closed.

An existing version succeeds only if config and ordered layer bytes match the
candidate, followed by full pull verification. A different config/layer digest
refuses overwrite. The workflow serializes all its publication jobs. Registries
provide no atomic tag compare-and-swap: operators must also exclude external
writers to the same repository during promotion. The helper rechecks before the
tag write and after pull verification; it cannot make arbitrary concurrent
external writers safe. Partial failure can leave content-addressed blobs or a
tag without a receipt; rerun the same verified candidate/version with a new
receipt path to finish verification. Never treat a missing receipt as success.

The receipt is a new file outside the immutable candidate and refuses overwrite.
Its schema is documented in [CANDIDATE-FORMAT.md](roadmap/operator-launch/CANDIDATE-FORMAT.md).
Keep it with operator records before the workflow artifact's 90-day retention
expires. It records a verified promotion, not deployment readiness or approval.

For local transport checks, run `python3 scripts/test-candidate-promotion.py`.
The optional real rehearsal requires the pinned `registry:2.8.3` image named by
that script to be present locally; it never implicitly pulls a dependency:

```bash
python3 scripts/test-candidate-promotion.py --local-registry /new/rehearsal-output
```

It creates one random task-owned registry container, bound to a random
`127.0.0.1` port with ephemeral storage, and removes only that container at exit.
A tiny loadable linux/amd64 image fixture tests upload, immutable pull, repeated
publication and conflict refusal without running the application. The dedicated
`--local-fixture` mode bypasses online provenance only for an explicitly numeric
`127.0.0.1:<port>` registry and marks all receipts `local-fixture`. It cannot
publish to GHCR and is never used by the Release workflow. These results are
transport development evidence; a final real candidate remains required.


## Publishing the original SDK candidate (#814)

After the release checklist and explicit npm publication authority are satisfied,
select the embedded package version and an unoccupied distribution tag:

```bash
gh workflow run release-sdk.yml --ref main \
  -f source_sha=<40-character-source-sha> \
  -f candidate_run=<successful-run-id> -f candidate_attempt=<attempt> \
  -f package_version=<embedded-sdk-version> -f distribution_tag=candidate-<version>
```

The workflow downloads the same v2 candidate and revalidates online provenance.
It publishes the original SDK tarball through `scripts/publish-candidate-sdk.py`,
never a source directory or new pack. Existing versions require exact SRI and
downloaded gzip bytes; occupied tags cannot be moved. Its independent
`sdk-publication-receipt` names the original tarball hash and verified package/
registry/tag identity. It neither publishes the image nor deploys the service.
The full token/relay/partial-failure runbook and local real-npm rehearsal commands
are in [the development guide](development.md#publishing-verify-oxsum-bill-to-npm).
No public publication is implied by locally successful fixtures or tooling CI.
