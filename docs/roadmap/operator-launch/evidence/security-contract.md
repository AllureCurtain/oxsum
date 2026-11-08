# OpenAPI credential authority repair (#800)

Baseline: `465061d`, after the current checkout-description correction in
[#804](https://github.com/AllureCurtain/oxsum/pull/804). This is a contract repair;
the server's authentication, capability checks and handlers are unchanged.

## Reviewed boundary

The initial text search found seventeen cookie/`apiKey` conjunctions. Parsing
the entire YAML found two more operations with the same undefined scheme in an
alternative requirement. The nineteen operations are all mounted behind
`require_principal` in [the router](../../../../crates/server/src/routes/mod.rs).
[The authentication module](../../../../crates/server/src/auth.rs) resolves an
explicit Authorization, x-api-key or x-goog-api-key header as a consumption key,
never as a personal session and never by cookie fallback. Personal sessions use
the `oxsum_session` cookie. The capability rules are defined in
[customer authority](../../../../crates/core/src/customer_authority.rs).

Paths below are relative to `/api/v1`. Each table row is one reviewed operation.

| Method | Path | Existing capability | Correct credential declaration |
| --- | --- | --- | --- |
| GET | `/plans` | ScopedRead | Cookie or any of the three consumption-key transports |
| GET | `/plans/{planVersionId}` | ScopedRead | Cookie or any of the three consumption-key transports |
| GET | `/bundles` | ScopedRead | Cookie or any of the three consumption-key transports |
| GET | `/org/promo-lots` | ScopedRead | Cookie or any of the three consumption-key transports |
| GET | `/org/plan` | Personal | Customer personal cookie |
| POST | `/org/plan/cancel` | Personal | Customer personal cookie |
| POST | `/org/plan/change` | Personal | Customer personal cookie |
| GET | `/org/statements` | Personal | Customer personal cookie |
| GET | `/org/statements/{statementId}` | Personal | Customer personal cookie |
| GET | `/org/plan/journals` | Personal | Customer personal cookie |
| GET | `/org/plan/journals/{journalId}` | Personal | Customer personal cookie |
| POST | `/orders` | Personal | Customer personal cookie |
| GET | `/orders` | Personal | Customer personal cookie |
| GET | `/orders/{orderId}` | Personal | Customer personal cookie |
| GET | `/orders/{orderId}/fulfillment` | Personal | Customer personal cookie |
| GET | `/orders/{orderId}/checkout-session` | Personal | Customer personal cookie |
| POST | `/orders/{orderId}/refund-request` | Personal | Customer personal cookie |
| GET | `/orders/{orderId}/refunds` | Personal | Customer personal cookie |
| GET | `/org/usage-report` | AccountManage | Customer owner/admin personal cookie |

The corresponding plan, bundle, organization, statement, journal, order, refund
and financial-fact handlers were checked for additional credential gates. The
commercial catalogues retain their published-version filters; private records
remain scoped by the resolved organization. This change does not turn the
existing promo-lot read into a new permission or narrow its runtime visibility.

The named consumption schemes are `bearer`, `apiKeyHeader` and
`googApiKeyHeader`. Each allowed credential is a separate OpenAPI `security`
object. The fifteen session-only operations now mark consumption access denied;
the eight missing 403 responses are declared, and the financial projection
states its additional owner/admin requirement.

## Validation

- The four Node contract checks run against parsed YAML. On the unchanged
  baseline they produced three failures and one pass, exposing all nineteen
  undefined references, all seventeen conjunctions and the incorrect route
  matrix. After correction all four pass with no skips.
- `npm --prefix web run codegen` passed, running those checks before generating
  the frontend contract. Generated changes are the eight existing 403 refusal
  shapes and the owner/admin description.
- `npm --prefix web run build` passed TypeScript compilation and the Vite
  production build. Vite retained its existing large-chunk advisory.
- A parsed before/after comparison confirmed that only the nineteen authority
  declarations, the missing 403 responses and the owner/admin description
  changed. Request fields, data schemas, unrelated operations and runtime
  sources are unchanged.
- `git diff --check` and local document-link checks passed. Final source review
  covered every row above and the code-generation/Docker invocation path.
- No Cargo, database or browser test was run. This evidence is contract and
  frontend validation, not release or runtime authorization acceptance.
