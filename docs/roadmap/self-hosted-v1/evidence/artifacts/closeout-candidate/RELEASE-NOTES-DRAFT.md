# Release candidate 813c788613f9f2a737562493c3b305ca1aaff5c1

Contract version: 0.122.0
Gated by workflow run https://github.com/AllureCurtain/oxsum/actions/runs/42338752086

## Changelog for 0.122.0

    0.122.0 adds the version-2 evidence bundle (issue #616): a whole-period
    customer-verifiable export — `POST /api/v1/org/evidence-bundles` mints a
    manifest-bound bundle committing usage records, plan statements, invoice
    journals, payment orders, refunds, disputes, credit notes and the
    day-seal digests; `GET …/{bundleId}` serves the stored bytes verbatim
    under a `data` envelope; `POST …/{bundleId}/share` mints a bearer
    `oxe-…` token (`expiresAt`/`maxDownloads`, revocable via
    `DELETE …/share`) that `GET /api/v1/evidence/{token}` resolves without
    a session — every dead-token case answers the same `410`. The offline
    `oxsum_verify::verify_evidence_bundle` re-digests the served manifest
    and artifacts and adopt-verifies each committed entry into the head.
    `upstreamCostMinor` redacts by default (`OXSUM_EVIDENCE_UPSTREAM_COST`
    opts in for internal-audit deployments).
