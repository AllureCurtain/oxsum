# Development artifact recovery rehearsal

`scripts/rehearse-artifact-recovery.py` exercises actual native binaries through
upgrade, shutdown drain, separate PostgreSQL restore and compatible rollback.
It takes explicit artifacts and identities; it never builds, publishes, chooses
a registry tag, adopts an existing database or supplies candidate provenance.
The report's tier is always `development-artifact-rehearsal`. Final N17/J16 and
N18 acceptance must repeat the relevant journeys against the frozen candidate
image and its full-CI provenance.

## Inputs and isolation

Supply three independently built native oxsum binaries: an old schema, the
selected current schema, and a retained artifact with exactly the current schema
and known behavior/data compatibility. The source SHA and SHA-256 of each input
are required. Source SHA is checked against the serving version endpoint;
byte identity is checked after copying into private frozen artifact files. A
source label is not build attestation: retain the build procedure and clean
source checkout evidence alongside the report.

Supply an original `verify-oxsum-bill` npm tarball and its SHA-256. The helper
freezes it, installs it with scripts disabled in a private clean consumer and
runs the existing offline verifier with networking disabled. No public package
is published. The package must contain the actual WASM module and dependencies
needed by the consumer; a placeholder package fails rather than skipping proof
verification.

The environment needs Docker, Node/npm, Python 3.10+ and native binaries for the
host. Two task-owned PostgreSQL 17 containers use a pinned image and ephemeral
numeric-loopback ports. They are independent clusters. A deterministic local
upstream uses a random loopback port and contains no external service credential.
Only invocation-owned processes, containers and anonymous volumes are removed.
The new output directory and retained evidence are private; transient keys,
sessions and logs remain in a separate temporary runtime directory.

```bash
python3 scripts/rehearse-artifact-recovery.py \
  --old-binary /private/artifacts/oxsum-old --old-sha <40-hex-source> \
  --old-sha256 <64-hex-binary-hash> --old-schema <older-schema> \
  --current-binary /private/artifacts/oxsum-current --current-sha <40-hex-source> \
  --current-sha256 <64-hex-binary-hash> --current-schema <current-schema> \
  --rollback-binary /private/artifacts/oxsum-rollback --rollback-sha <40-hex-source> \
  --rollback-sha256 <64-hex-binary-hash> --rollback-schema <current-schema> \
  --sdk /private/artifacts/verify-oxsum-bill.tgz --sdk-sha256 <64-hex-tar-hash> \
  --confirm-compatible-rollback --output /new/private/recovery-evidence
```

`--confirm-compatible-rollback` is a deliberate operator fixture decision; equal
schema alone is not proof of behavior compatibility. The helper requires three
distinct source identities, an actual schema advance for upgrade, and an exact
schema match for rollback. It has no schema downgrade path. Package version
currently defaults to `0.1.0` and can be supplied with `--version`.

## What the run checks

1. Start the actual old binary and verify exact source/package/schema readiness.
   Use the existing owner/customer fixture to provision two distinct customer
   accounts, immutable sale books and declared synthetic manual funding. Make
   actual streamed gateway calls; each one must settle 34 integer minor units
   once with one upstream attempt and no leaked reservation.
2. Capture a genuine bill, expected content hash, held head, signed current head,
   price/sale binding, consistency proof and retained public signing key. The
   key is trusted as an isolated fixture input; it is not an independent
   attestation of an operator's real-world identity.
3. Hold a streaming request at the upstream, observe an active reservation, send
   SIGTERM, confirm the process remains alive while the stream is blocked, then
   release it inside the documented nine-second drain window. Require a complete
   SSE response and clean process exit. Verify the final settlement after upgrade.
4. Run the current binary's real migration command and start it on the same source
   database. Check exact runtime identity, both customers' balances, the original
   customer credential and public signing key. A new gateway call exercises the
   retained sealed provider credential through normal preparation/serving.
5. Stop the current process, use the production snapshot-bound backup/restore
   helper to restore into the second PostgreSQL environment, and start the same
   current artifact there. Re-export a proof for the old entry, capture a new
   restored call's proof, and check both customers and exact balances again.
6. Stop the current artifact and start the explicit compatible artifact on the
   restored same-schema database. Require unchanged signing identity and another
   successful exactly settled request. Stop every application process, then
   verify the original, re-exported old and new restored evidence offline using
   the frozen SDK, including its existing altered-input rejection matrix.

The final JSON lists exact artifact hashes/source identities, stage results,
measured migration/drain timings, backup/restore receipts and outcome. Separate
checksums cover every retained artifact/evidence file. A failed run records its
stage; it never reports success based only on process liveness. Private runtime
logs and credentials are destroyed with the temporary runtime directory, so
rerun an identified failure with a reviewed local debugging setup if more detail
is required. Do not upload binaries, dumps or customer evidence indiscriminately.

This is a small synthetic multi-customer exercise. It does not cover a production
capacity dataset, arbitrary previous versions, unknown compatibility, all active
operation crash paths, external witnesses, PSP/provider certification, browser
journeys or a final candidate image. Its timing and successful balances do not
constitute a production RPO/RTO or uptime guarantee.
