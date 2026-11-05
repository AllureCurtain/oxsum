# Candidate compatibility notes

This file accumulates changes for the self-hosted candidate; it is not a release announcement.

## Customer manual top-up removed (#624)

Authenticated POST /api/v1/topups now returns 403 without changing the ledger. Customers must use configured checkout or issued redemption codes. Operators recording verified external receipts use POST /api/v1/admin/organizations/{organizationId}/manual-receipts with amountMinor, idempotencyKey, unique reference and reason. Repeat identical terms to recover an interrupted response. Never grant purchased funds merely because a browser returned from checkout. Promotional adjustments remain distinct from cash receipts.

Existing balances, organization/key IDs, ledger origins, entries and proofs remain unchanged. Raw customer hold/settlement authority is tracked separately in T01.b; this child change alone does not complete T01 or the release.

## T01.b — Customer raw reservation and settlement removed (#625)

`POST /api/v1/holds` and `POST /api/v1/settlements` now return 403 for authenticated customer sessions/API keys before parsing the body. Integrations must use the gateway for consumption or an explicitly provisioned service credential for `/api/v1/metering`. Customers cannot release an existing gateway hold or set the actual charge. `GET /api/v1/holds`, existing evidence, legacy hold sweeping and authorized service-metering operations remain supported. The overview's manual hold bench is removed in both locales.


## Independent customer admission restrictions (#633)

Manual `suspended: false` on the deployment organization PATCH now clears only manual suspension. It cannot override dunning or archived customer state. Reconcile overdue payment or disable the configured dunning policy to remove its reason, and explicitly restore an archived customer. Existing balances, IDs, historical evidence and active settlement are preserved.
