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

The typed frontend depends on the #846 application contract. Initial local
request-building checks pass (3): fixed application authority and absolute
expiry, legacy scope/customer separation, and malformed expiry/authority refusal.
The production TypeScript/Vite build passed against the prepared #846 generated
contract; the temporary schema copy was restored afterward. Final integrated
checks remain pending until #846 merges. This is not browser
acceptance: the original task browser space remains unavailable. Desktop/mobile,
keyboard, locale and response-loss journeys must run on the actual candidate.
