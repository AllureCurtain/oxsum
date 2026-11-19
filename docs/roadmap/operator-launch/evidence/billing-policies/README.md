# Versioned billing policies acceptance (#1235)

Local synthetic PostgreSQL acceptance, 2026-11-20. This is engineering evidence,
not customer consent on a production installation or provider certification.

- Full workspace: 2,609 passed, 6 configured skips, 522.560 seconds. Command:
  `DATABASE_URL=<local PostgreSQL> cargo nextest run --workspace`.
- Seven core consent/admission tests cover publication versus acceptance, exact
  reviewed customer identity, revoked authority, concurrent acceptance/admission,
  account deletion lock order, immutable digest, forged policy fields, current
  selection outside the 100-version history page, and restart retention.
- HTTP exercises all four endpoints, personal customer authority, actual gateway
  binding, subsequent publication and receipt interpretation after diagnostics
  deletion. A cache regression proves explicit legacy consent preserves hits.
- Pinned formatting, strict core/server Clippy, OpenAPI contracts/codegen and
  frontend build passed on the integrated correction base `ed5af595`.
- Independent agent `review_recovery_supplier` approved the integrated diff
  after fixes; review was static, not a claim to have run these checks.

Browser: local backend plus Vite, synthetic `oxsum_correction_browser` database.
Published provider-reported version 1 and explicit legacy version 2 through
Business Prices. Customer Billing still stated no explicit acceptance. Version 1
was visibly unavailable in this intermediate runtime. Version 2 review named the
customer, scope, estimator and exact digest; explicit acceptance survived reload.
The 390px viewport had a 390px document width. Both screenshots were inspected.
HTTP tests separately cover durable receipt binding for a newly admitted request.

The provider-reported offer remains `runtimeReady=false` until #1236 implements
its writer. Publication alone cannot migrate customers; historical settlement
and sale snapshot encodings are unchanged. Existing verifier proof scope does
not include authenticated policy consent.
