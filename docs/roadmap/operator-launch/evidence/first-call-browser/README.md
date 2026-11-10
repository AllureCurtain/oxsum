# Integrated customer readiness and first-call browser acceptance

Issue #960; continuation of #946. Runtime source
`ac7bb191ee7a2b52af4a23f2ce54cd9c6f9d82e5`, schema
`0128_application_notifications`, binary SHA-256
`941e9f1776e64ed5559074c637bf73d67de7380a382dc12f56e1b149ca610c53`.
The source includes #916, #940, #943, #950, #952, #956 and #958; later dependency
updates on main are not silently included in this runtime evidence.

The existing isolated #946 customer and local SMTP/model fixtures were reused.
No real provider/payment request or balance grant was made. Shared real-provider
budget remains 2/5 completed attempts from the separate #944 evidence.

## Browser actions and observed transitions

1. With a zero wallet and paid customer rates, Chat returned HTTP 402. The
   integrated getting-started page showed 2/5 ready: model and live key were
   available, but the illustrative 192-minor reservation lacked funds. It did
   not ask the customer for a platform operator token.
2. The owner used Channels to replace the owned loopback destination with an
   SSE-capable local model, explicitly approving only `127.0.0.1`. Then Prices
   created an immediate customer-specific `browser-chat` override with explicit
   zero input/output rates and max output 32. The business default paid version
   remained unchanged and the override was visible in customer history.
3. Customer sign-in and reload showed 3/5 ready, reservation/quota/wallet all 0.
   The user sent `Hello` through Chat. The gateway streamed `Local acceptance
   response`, then completed. Getting-started showed 5/5 and an immediate link
   to entry `6c3b5e54-03bc-5b94-aa86-bb20c8f6f3c5`.
4. The exact link loaded the customer-scoped original proof and expected hash,
   explicitly describing same-server hash trust. Verify passed. The recorded
   charge, price checkpoint and customer sale checkpoint all verified.
5. A wrong all-zero expected hash failed. The actual served WASM module was
   loaded and returned `{ok:true, verdict:"pass"}` for the original and
   `{ok:false, verdict:"fail"}` for the wrong hash. The browser network was then
   set offline; the visible Verify form still produced the passing result.
   Networking was restored afterward. Thus the final success was computed
   locally, not by a server fallback.

The raw disposable bill and expected hash are retained beside the screenshots,
with SHA-256 inventory. No receipt secret, API key, session cookie or invitation
is included. Screenshot contents were visually inspected. Staff-only membership
was also checked read-only: Finance and Support had zero customer memberships;
only the original owner/customer organizations existed.

## Scope

This closes the exercised free-call/zero-wallet development path for customer
readiness and immediate proof discovery. It does not certify paid collection,
plan quota, team scoping, commercial refund/dispute/statement browser journeys,
real-provider token truth, independent head trust, external PSP, final candidate,
reference capacity/soak or independent pilots. The sale checkpoint binds the
operator's declared customer terms; it does not certify an external invoice.
