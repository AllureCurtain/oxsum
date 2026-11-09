# Application frozen evidence reads

Issue #924; N21/J18. Dependency PR #922 (`309edd88`); migration 0124 follows
reserved 0120–0123. Local PostgreSQL 17 and loopback HTTP fixtures only.

An explicit application-only `evidenceRead` scope lists retained descriptors in
bounded UUID pages and downloads original customer-safe version-2 evidence bytes.
Authorization and data reads use one repeatable-read transaction, holding live
credential/namespace authority through the read. No wallet is opened, no evidence
marker or share token is minted, and no domain write or provider action occurs.
Archived customers retain historical access while the grant remains live.

Downloads enforce 16 MiB before transferring bytea from PostgreSQL, exact descriptor
identity, manifest/entry/inclusion/signature integrity, and no supplier costs.
Duplicate JSON keys and uncommitted artifact additions refuse. The bytes returned
are unchanged, including whitespace. Retained key verification establishes internal
consistency only; the independent consumer must hold its own origin/key/digest.
HTTP returns application/json as an attachment with private no-store and nosniff.
Descriptors are metadata, not validity or downloadability verdicts.

Executed development validation:

- 17 focused core/HTTP checks passed (3.784s) across evidence, consent and entitlement
  reads. Five final core/HTTP checks passed (1.428s), including a nonempty signed
  statement export, strict duplicate-key decoding and optional-field type checks.
  Four core cases passed again after the Clippy-only array cleanup (1.358s).
- Real signed frozen bundles survive byte-preserving download, pagination,
  archive, credential rotation and one-connection operation without ledger/domain
  writes. Foreign account/bundle, missing scope, expired/revoked credential,
  cost-inclusive, malformed, extra artifact, modified entry/signature and
  oversized retention refuse. HTTP covers both routes and file headers.
- Four OpenAPI checks, generated TypeScript, frontend production build and seven
  frontend grant/fragment behavior checks passed. Existing bundle-size warning.
- All 2,168 ordinary workspace tests passed (323.015s); six existing opt-ins
  remained skipped. The added statement fixture brings the final selected source
  set to 2,169; unchanged full-run evidence is reused. Strict core/server
  all-target Clippy, pinned formatting and diff checks passed.

Independent review/protected integration and owner scope-selector browser checks
remain pending. The earlier browser task space is unavailable; no replacement or
user-owned space was claimed. No candidate, independent pilot or external payment/
provider certification follows from these fixtures. N21 checkout, automated evidence
publication, signed notifications and complete external application remain separate.
