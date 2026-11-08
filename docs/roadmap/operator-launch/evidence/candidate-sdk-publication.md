# Original SDK tarball publication development evidence (#814)

Date: 2026-11-08. Scope: N13.d. Local registry transport was exercised with real
npm; no public registry request, package version bump, public publication, image
build or application deployment occurred. Final candidate/J14 acceptance remains
separate.

## Implemented contract

The manual Release SDK workflow requires source SHA, successful candidate run/
attempt, embedded package version and an explicit distribution tag. It downloads
and revalidates the v2 bundle online, freezes a private snapshot and gives only
the original SDK `.tgz` to real npm. Lifecycle scripts and npm provenance
generation are disabled. Isolated config/cache/cwd/environment prevent ambient
registry/proxy/Node preload overrides, and publishConfig overrides are refused.

npm receives an ephemeral loopback registry/token. The bounded relay accepts
only the selected package GET and one validated PUT, verifies the attachment's
original bytes, length, SRI and SHA1 metadata, then sends the exact attachment to
the selected upstream. Only metadata's tarball URL changes. The remote token
stays in Python, is sent to one exact origin, and never follows redirects. The
local fixture mode accepts only numeric `http://127.0.0.1:<port>/` and marks
receipts local-fixture; production requires HTTPS registry.npmjs.org and online
candidate provenance.

Version/tag preflight refuses different existing bytes or an occupied tag.
Idempotent success requires the same version/tag, SRI and downloaded original
bytes. A successful independent receipt binds source/workflow/manifest/tarball
hashes and registry/package/version/tag. External writers must honor the same
publication exclusion because npm tags have no atomic compare-and-swap. A partial
publish without receipt remains incomplete; absent-tag repair is explicit work.

## Executed checks

| Check | Result |
| --- | --- |
| CI evidence tests | 7 passed |
| Candidate bundle tests | 15 passed |
| Image promotion tests | 17 passed |
| Digest deployment tests | 16 passed |
| SDK publication tests | 11 passed |
| actionlint 1.7.12 SDK/CI workflows | passed |
| Pinned format / diff whitespace checks | passed |
| Real npm 11.19.1 local publication | original gzip bytes uploaded/downloaded |
| Identical retry | already-present; only one registry PUT |
| Same version, changed bytes | refused before a second PUT |
| New version, occupied tag | refused before a second PUT |
| prepare/prepack/prepublishOnly/publish/postpublish sentinels | none executed |
| Hostile ambient npm registry/config and Node preload | stripped; local operation passed |
| Registry 307 redirect during real npm PUT | refused; second listener saw zero requests/credentials |

The independent retained rehearsal used
`python3 scripts/test-candidate-sdk.py --local-registry /tmp/oxsum-sdk-publication-rehearsal-814`.
It retained synthetic candidate bundles, `published.json`, `repeated.json` and
`rehearsal.json`. All successful receipts are `local-fixture`; their synthetic
source/CI values are not release provenance. The actual run's original fixture
SDK bytes were:

```
package/version: verify-oxsum-bill@0.1.0
SHA256: 83ea6887133d3f3fe72ae51866f30ceceb96975e4cdb5d30d74dbcdbfe979ece
SRI: sha512-eeqLFkfM9bH7dxziQ87pCrSmZSsvIdK8VR75o65bzFwgROKHBcy3jaRurSWApFrs3XEjag06Q8XZNWqt2qvKSQ==
```

Only ephemeral loopback HTTP endpoints were started, then shut down. No Docker
image, database, root environment file, user npmrc or shared Cargo output was
changed. The minimal fixture registry is not a certification of npmjs.org's live
service or authentication policies. A real successful unexpired candidate,
explicit npm publication authority and actual post-publication verification are
still required before claiming a release.
