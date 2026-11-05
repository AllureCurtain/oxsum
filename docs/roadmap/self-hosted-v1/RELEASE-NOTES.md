# Candidate compatibility notes

This file accumulates changes for the self-hosted candidate; it is not a release announcement.

## Customer manual top-up removed (#624)

Authenticated POST /api/v1/topups now returns 403 without changing the ledger. Customers must use configured checkout or issued redemption codes. Operators recording verified external receipts use POST /api/v1/admin/organizations/{organizationId}/manual-receipts with amountMinor, idempotencyKey, unique reference and reason. Repeat identical terms to recover an interrupted response. Never grant purchased funds merely because a browser returned from checkout. Promotional adjustments remain distinct from cash receipts.

Existing balances, organization/key IDs, ledger origins, entries and proofs remain unchanged. Raw customer hold/settlement authority is tracked separately in T01.b; this child change alone does not complete T01 or the release.

## T01.b — Customer raw reservation and settlement removed (#625)

`POST /api/v1/holds` and `POST /api/v1/settlements` now return 403 for authenticated customer sessions/API keys before parsing the body. Integrations must use the gateway for consumption or an explicitly provisioned service credential for `/api/v1/metering`. Customers cannot release an existing gateway hold or set the actual charge. `GET /api/v1/holds`, existing evidence, legacy hold sweeping and authorized service-metering operations remain supported. The overview's manual hold bench is removed in both locales.


## Independent customer admission restrictions (#633)

Manual `suspended: false` on the deployment organization PATCH now clears only manual suspension. It cannot override dunning or archived customer state. Reconcile overdue payment or disable the configured dunning policy to remove its reason, and explicitly restore an archived customer. Existing balances, IDs, historical evidence and active settlement are preserved.

- **Customer joining consent (#634):** `POST /api/v1/org/members` now returns 403. Use `POST /org/invitations` with an idempotency key and optional bound email, then explicit `/invitations/accept` for signed-in users or `/invitations/redeem` for new users. Creation retries return null token and do not resend mail. Existing unspent links survive upgrade. New business customer invitations reserve external identity until intended-recipient consent; no staff or funds are granted.


## Scoped consumption keys (#637)

Existing keys retain inference and own evidence/status access but lose account administration, payment initiation, balances/budgets and customer-wide history. Use a current personal session for keys/mandates/team/checkout; statements, reports/share links, experiments and webhooks require owner/admin. Members' raw entries/proofs/exports/sockets follow their key attribution and shared unattributed history, excluding account-wide document markers. Revocation does not hide a member's historic key receipts.

The balance response header is removed. Tasks, response caches and replay records are exact-key scoped. A legacy replay record conflicts until its original 24-hour expiry; pre-upgrade cache rows miss. Existing key IDs/secrets, ledger balances, origins, entries and downloaded proofs remain unchanged. T05.b integration/metering credentials remain a separate required child, #638.

- The public verification example no longer reads a customer selected by display name. `/verify?sample` uses fixed public test evidence, labeled as such, even when a customer calls itself `Acme Demo`.


## Scoped business integrations (#638)

Legacy global service secrets are disabled by migration 0077, and deployment `POST /admin/service-credentials` returns 403. Preserve old metadata/ledger evidence. The current business owner must issue an oxs-int- integration credential with selected customers, customersRead and/or meteringWrite, expiry and an audited reason. The secret is shown once; retries return null. Existing old holds remain recoverable by trusted jobs and are not transferred to replacement credentials.

Save the metering response's opaque holdKey instead of deriving the old `metering:<key>:hold` spelling. Replays require the same credential, customer, operation and body. eventId remains the external label in settlement evidence, while usage/sweeper request identity uses the scoped hold key; repeated labels across customers no longer suppress usage rows. Prices/discount and bound remain pinned for retries. Broader management scopes and the complete business console remain later release work.
