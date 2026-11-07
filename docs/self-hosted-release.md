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
| 3 | `release.yml` — build + push `latest`, `sha-<sha>`, optional version tag | manual dispatch | the reviewed candidate SHA |
| 4 | `deploy.yml` — publish + deploy | manual dispatch, separately authorized | a published tag |

Gate 1 fails closed unless `check-candidate-ci.py` finds a successful
completed `ci.yml` run on exactly the candidate SHA and main branch. Every
shard and the process-shared and engine-conformance steps must have executed
successfully. A green path-filtered run or a run that skipped conformance is
insufficient. Run `gh workflow run ci.yml --ref main` while main points to the
chosen candidate, wait for it to finish, then dispatch the candidate gate.
The validated CI URL, attempt and job verdicts are retained in
`candidate-ci.json` and linked from the candidate manifest. Fixture tests in
`test-candidate-ci.py` cover failed, missing, skipped and wrong-SHA evidence.

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
  The exact tarball it verified uploads as `verify-oxsum-bill-tarball`.
- **examples** — `scripts/verify-examples.sh` boots a scratch deployment
  (postgres service + the server built at the promoted SHA) and runs every
  shipped developer example end to end — the A27 fresh-install half.
- **image** — the full Dockerfile at the promoted SHA with
  `GIT_SHA=<sha>` baked in (so `/api/v1/version` inside the image answers
  the gated commit). Loaded into the runner's daemon and started through the throwaway Compose smoke. The smoke checks its
  reported SHA, migration, readiness, business setup and customer invitation.
  **Never pushed**: no registry login exists in this workflow. The tested image
  is exported as `candidate-image.tar.gz`; `docker load` restores the exact
  bytes for repeat validation. `candidate-smoke.log` records the executed run.
- **manifest** — `candidate-manifest.json` recording the SHA, run URL,
  image identity, full-CI URL and per-gate verdicts; `SHA256SUMS` over every artifact;
  `RELEASE-NOTES-DRAFT.md` cut from the candidate's own `openapi.yaml`
  changelog paragraph for its contract version. All three plus every
  artifact upload as `candidate-bundle` (90-day retention).

## What gate 2 proves

`scripts/release-candidate-smoke.sh` runs the image on a throwaway compose
project — its own postgres volume, generated secrets, full teardown. The
profile and env file live in a temporary directory; `deploy/.env` is never
modified. The image job runs this gate with `SMOKE_BUILD=0`; the mandatory
verify-consumer job covers the SDK on the same SHA. To repeat it locally after
loading the candidate-image artifact:

```bash
OXSUM_IMAGE_TAG=sha-<sha> SMOKE_EXPECTED_SHA=<sha> SMOKE_BUILD=0 scripts/release-candidate-smoke.sh
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
3. After explicit publication authorization, select a branch/tag whose
   resolved commit is exactly the approved SHA and dispatch
   `gh workflow run release.yml --ref <reviewed-ref> -f image_tag=v<x.y.z>`.
   This workflow rebuilds its selected ref; `image_tag` is only an output
   label, not a source selector, and it does not promote the saved tar
   byte-for-byte. Verify the selected ref and resulting image SHA/digest.
   An unqualified dispatch against a moving main can publish different code.
   This publishes `latest`, `sha-<sha>` and the version tag to GHCR; no such
   dispatch is part of candidate validation.
4. Deploy stays separate: `deploy.yml` calls `release.yml` and adds the
   deploy job — a separately authorized, manual action (gate 4/G5 in
   ACCEPTANCE.md).

An upgrade to the published tag follows `docs/self-hosted-upgrade.md`:
`OXSUM_IMAGE_TAG=v<x.y.z>` in `deploy/.env`, the `oxsum-migrate` one-shot
runs ahead of the serving container, `/api/v1/version` confirms the
landed build.
