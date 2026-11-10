# Verified application account consent

Issue #920; N21 / J18. Dependency: PR #910 (`68f405fd`). Migration 0123
follows reserved 0120/0121/0122; integrate in that order for a candidate.
Environment: local PostgreSQL 17, isolated scratch databases, local HTTP fixtures.
No external mail, payment/provider request, deployment or spend.

The new explicit application-only `customerLinkWrite` scope creates/revokes an
immutable seven-day email-bound invitation for a namespace-owned unclaimed
account. Hash-only secrets appear once; exact retry returns metadata. Rotated
current authority can revoke, but a revoked/expired issuer cannot authorize
acceptance. Personal preview and acceptance require the independently verified
recipient email and current second factor. Acceptance atomically records first
ownership, append-only consent and audit, with no account switch, key/staff grant,
ledger transfer or money movement. Replay after membership removal cannot restore
permission, and already accepted accounts cannot be claimed again.

English/Chinese `/app/application-link` reviews the named application and account,
discloses retained application access and requires explicit confirmation. Secrets
are fragment-only until JSON submission, excluded from query-cache keys, retained
only in bounded tab-local storage across login, and removed after success/discard.

Executed development checks:

- 32 focused core/HTTP tests passed with no skips (6.307 seconds), including all
  four endpoints, real local email-token verification, wrong scope/namespace,
  revoked/expired credentials, wrong/unverified email, TOTP, duplicate/different
  concurrent creates, concurrent accepts, revoke/accept, immutable history,
  existing members, audit rollback and terminal replay without regranting.
- Six frontend behavior tests passed: fragment/storage validation, expiry,
  replacement refusal and application-only grant construction.
- Four OpenAPI authority checks, generated TypeScript, production frontend build,
  pinned formatting and route coverage (85 mounted / 85 classified) passed.
- Full `cargo nextest run --workspace --no-fail-fast`: 2,164 passed, six existing
  opt-ins skipped, 372.877 seconds. Final credential-expiry recheck after waiting
  for membership then passed all six affected core/HTTP tests (1.944 seconds).
- Strict all-target Clippy for `oxsum-core` and `oxsum-server` passed. Frontend
  build retains its existing bundle-size warning. No dependency versions changed.

Actual browser desktop/mobile/keyboard/refresh/locales remain unperformed for this
flow: the task's prior Ego TaskSpace disappeared, and the runtime forbids creating
a replacement to bypass that condition without the user's authorization. The
software checks above are not browser acceptance. Independent review, protected
integration, complete external application and exact-candidate gates remain.
