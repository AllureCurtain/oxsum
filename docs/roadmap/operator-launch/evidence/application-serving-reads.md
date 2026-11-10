# Application entitlement and customer sale reads

Issue: #898; N21 / J18. Base: `5285fc0b3bc825b0707bba675ea5d1ff0efa903d`.
Environment: local PostgreSQL 17 and local fixtures; no provider/PSP calls.

The application-only `entitlementsRead` grant reads recorded plan/quota/debt
standing and bounded permitted public models with their published customer sale
terms. Each read rechecks credential, namespace and customer authority. An
optional consumption key must be current and issued by that exact application
for that customer. Existing grants gain no permission.

Reads use one repeatable-read snapshot, retain the credential revocation lock,
and perform no domain writes. Ended quota windows are labeled, not rolled;
pending draws remain included in recorded consumption. Customer overrides,
assigned books and default books use the same resolver as admission without
recording sale digests. Missing terms stay absent rather than falling back to
wholesale prices. Provider identifiers, credentials and costs are not returned.

Executed validation:

- 21 focused core/HTTP checks across application serving reads, application key
  lifecycle and sale books passed, including revoked/expired/wrong-namespace
  credentials, aliases, groups, keys, pagination, zero/missing/future prices,
  pending/terminal plans, quota reservations, shared debt and mutation snapshots.
- Full `cargo nextest run --workspace`: **2,158 passed, 6 existing opt-ins
  skipped**, 407.452 seconds test runtime; real scratch PostgreSQL databases.
- Strict all-target Clippy for `oxsum-core` and `oxsum-server` passed.
- Four OpenAPI checks, four grant-form behavior checks, generated frontend
  contract, frontend production build and pinned formatting passed.

Independent review and protected PR integration are pending. Browser journeys,
the complete external application example and exact-candidate acceptance remain
separate gates; these development results do not certify N21 or release readiness.
