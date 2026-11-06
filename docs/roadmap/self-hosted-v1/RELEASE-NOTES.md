# Candidate compatibility notes

This file accumulates changes for the self-hosted candidate; it is not a release announcement.

## Customer manual top-up removed (#624)

Authenticated POST /api/v1/topups now returns 403 without changing the ledger. Customers must use configured checkout or issued redemption codes. Operators recording verified external receipts use POST /api/v1/admin/organizations/{organizationId}/manual-receipts with amountMinor, idempotencyKey, unique reference and reason. Repeat identical terms to recover an interrupted response. Never grant purchased funds merely because a browser returned from checkout. Promotional adjustments remain distinct from cash receipts.

Existing balances, organization/key IDs, ledger origins, entries and proofs remain unchanged. Raw customer hold/settlement authority is tracked separately in T01.b; this child change alone does not complete T01 or the release.
