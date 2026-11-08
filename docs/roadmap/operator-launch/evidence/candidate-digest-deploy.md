# Candidate digest deployment development evidence (#810)

Date: 2026-11-08. Scope: N13.c. This evidence covers tooling and controlled
orchestration tests. No oxsum image was run, no public registry was written, no
SSH connection was made, and J14 remains open for real candidate acceptance.

## Implemented

- One required `OXSUM_IMAGE_REF` controls both Compose migration and serving;
  absent input fails rendering. Local candidate smoke can explicitly choose its
  pre-publication tag and retains actual runtime JSON as hashed `runtimeVersion`.
- Version-2 candidate bundles remain promotion-compatible without that optional
  field, but digest deployment refuses missing runtime source/package/schema.
- `deployment_plan.py` freezes and validates the candidate online, verifies the
  selected GHCR digest and every registry/config/layer byte with read-only
  authority, and writes a new plan. A plan is not a signature; trusted preparation
  and authenticated transfer are mandatory.
- Deploy no longer calls Release. Strict selectors, minimal read permissions and
  pinned SSH known hosts precede plan/helper transfer to the configured host.
- The host helper locks private state, refuses unmanaged containers or drift,
  pulls/saves/checks image config and rootfs before downtime, freezes one operator
  environment for migration/serving, then checks exact readiness and all runtime
  fields. Success receipts retain current/previous identities and environment
  hashes. Failures do not fabricate receipts or trigger automatic rollback.
- Rollback selects only a retained previous success, with unchanged previous/
  current environment, exact same schema and an explicit behavior-compatibility
  decision. Other cases require separate database restoration.

## Checks executed

| Check | Result |
| --- | --- |
| Candidate CI evidence suite | 7 passed |
| Candidate bundle suite | 15 passed |
| Image promotion suite | 17 passed |
| Digest deployment suite | 16 passed |
| Compose config with explicit digest | migrator/server render the same ref |
| Compose config without image ref | refused |
| `bash -n` smoke/config helpers | passed |
| actionlint 1.7.12 deploy/candidate/CI workflows | passed |
| Pinned `scripts/fmt-check.sh` / `git diff --check` | passed |

Deployment tests cover missing runtime evidence, failed provenance, wrong
source/package/schema/platform, mutable and foreign refs, fixture-tier refusal,
read-only registry write refusal, unmanaged target refusal, wrong pulled image,
migration/start/readiness failures without success, retained artifact selection,
compatibility opt-in and mismatched current/previous operator environment.

The compatible-rollback orchestration test starts an actual random-port
`127.0.0.1` HTTP server for readiness/version responses. Docker commands are
mocked as an explicit container state model: first deployment, second immutable
reference and rollback select the exact expected references for both migration
and serving, and current/previous receipts transition correctly. The synthetic
plans and receipts exist only in the test's temporary directory and are removed.
This does not prove Docker pull, application startup, migration or a real
candidate. Its `verified-candidate` fields are controlled test inputs, never
retained as release evidence.

## Limits and remaining acceptance

The local image inventory contains no oxsum image. Docker Desktop's daemon
loopback differs from the host loopback used by the successful N13.b registry
transport rehearsal. Production digest semantics were not weakened to force a
mock image through that boundary. A real candidate archive must be generated
with runtimeVersion evidence, promoted within its authorized scope, and used for
actual digest deployment/readiness/version and compatible rollback. A candidate
with a different schema requires the restore rehearsal on a separate database.

The local developer platform is arm64 Docker Desktop, not the Linux/amd64
reference host. No Cargo build or database mutation was used in this unit.
Required PR CI and independent review remain separate from these development
results. N13.d original SDK tar publication and final integrated candidate gates
are not covered by this change.
