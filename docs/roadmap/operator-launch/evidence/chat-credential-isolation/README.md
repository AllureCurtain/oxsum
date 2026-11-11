# Browser chat credential isolation

Issue #990. The original global `oxsum.chat-key` cache reused the prior customer's
secret whenever the current customer had any live key named `chat`. Actual A/B/A
login reproduced a read-only mismatch: session customer `57ae06a2…` while the
credential resolved to `c3dc85ec…`. No cross-customer inference was sent after the
mismatch was discovered. The baseline retains only identities, never secrets.

The repair stores credentials under exact organization/user identity, matches
one live key ID and creator, checks its credential organization without cookies,
and ignores the legacy slot. Initialization deduplicates effect replay, verifies
identity around asynchronous work and discards late results. Chat state is keyed
by customer/user; model loads and active requests abort on unmount. Sending chat
or a probe checks the current session before contacting the gateway.

Five focused tests cover A/B/A and same-customer different-user isolation,
revoked/expired/deleted/foreign keys, concurrent initialization, late creation
after identity change and pre-send mismatch. Five contract checks, production
TypeScript/build, pinned formatting and diff checks passed.

Actual browser evidence on frontend `0754da19` confirmed A/B/A organization and
key identities, exact reuse of A's own key, then a successful 40-minor local SSE
call attributed to A's key in its billing records. Revoking that key and reloading
created a different, correctly scoped key. Final frontend `ad693559` adds the
localized account-change explanation and preserves that implementation. On it,
a fixture switched the actual session cookie to B while A's page stayed open;
sending was refused before **any gateway request**. English and Chinese/390px
screenshots show the refusal. Reload selected B's correct credential. All
published screenshots were visually inspected.

Backend source `ca835528c5001434ae24256c177f1adb50bed274` and final frontend hashes
are in `runtime.json`. Local isolated model only; no real-provider budget change.
These checks prove the exercised browser ownership transitions, not independent
PSP, full-candidate or external pilot completion. Existing unscoped legacy keys
are ignored, not silently reused or deleted from the server.
