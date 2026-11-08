# Owner application and credential configuration (#853)

The old portal form omitted required customer IDs and expiry and generated a new
key on every submit. The new owner-only section creates and selects immutable
applications, chooses application or named-customer authority, explains the exact
scopes, and submits a typed contract with expiry and an audit reason.

A submitted body is retained in component memory, including absolute expiry and
selected identity. Pending clicks are guarded synchronously; retries keep the
same key and body. Changed intent requires an explicit new operation. The secret
is displayed once with a copy action, and a metadata-only retry explains recovery
through revocation and replacement. It is never written into local storage,
query parameters or logs. Reload does not submit anything. Application and
credential lists and customer selection are paginated; issued metadata
separates active, expired and revoked states. Revocation uses an explicit dialog
and retains its submitted reason/key through errors and reopening.

The final frontend is based on #846 integrated as `42b80a24`. Three request-building
checks passed: fixed application authority and absolute expiry, legacy
scope/customer separation, and malformed expiry/authority refusal. Four OpenAPI
security checks, generated types, TypeScript/Vite production build, repository
formatting and diff checks passed. The generated schema remains unchanged.
Independent source review passed for authority, frozen retry identity, duplicate
click prevention, labels after selection changes and secret/revocation recovery.
The page uses the shared Checkbox primitive, including explicit scope disabling.

This frontend-only unit does not change Rust runtime behavior; the integrated
#846 source already passed 2,003 full workspace tests. Browser acceptance remains
pending: the original task browser space is unavailable. Desktop/mobile, keyboard,
locale and response-loss journeys must run on the actual candidate.
