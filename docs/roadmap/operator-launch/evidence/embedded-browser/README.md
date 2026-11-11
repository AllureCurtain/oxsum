# Browser application handoff (#976)

Date: 2026-11-10. Parent #946; complements the separate-process automated
[application walkthrough](../embedded-application.md). Uses the same development
binary/frontend as [commercial browser acceptance](../commercial-browser/README.md).
No external provider, PSP, email recipient or receiver was contacted.

## Owner configuration and independent application

Through Business → Integrations, the owner created `browser-embedded` / Browser
Embedded Application (`af71d71a-029c-4d24-9483-41cfe74d334f`), retained limit five.
The owner minted a one-day application-only credential with customersRead,
customersProvision, billingRead, customersLifecycleWrite, customerKeysWrite,
entitlementsRead, customerLinkWrite, evidenceRead, checkoutWrite, evidenceWrite
and notificationsRead. External-work metering was not granted. One-time material
was saved privately and disappeared after reload; tracked evidence contains no
credentials, invitation tokens, signing secrets or signature headers.

The actual Python example ran in a separate process with its own private SQLite
store and the machine credential, without owner cookies. Signup retained external
identity `linked-browser` and customer `92b22f0e-1fc8-402b-9349-a157016c33f7`.
It then created a seven-day invitation for the existing verified portal customer.

## Explicit human consent

The fragment-only invitation was opened in the browser and removed from the URL.
An unverified owner identity saw the verification requirement and no acceptance
control. Signing out and signing in as the intended verified customer preserved
the invitation within the tab. The preview disclosed the exact application,
account, recipient and retained application access; acceptance required checking
the confirmation box.

After acceptance the original customer account
`57ae06a2-47b1-4989-806b-69a8229e4569` remained selected. The account menu offered the
newly linked account as owner; the customer explicitly selected it. Both retained
session-context responses show customer context and no business membership. The
linked account had zero funds, and the application credential still returned 404
for the original personal account while retaining 200 access to its own account.
Consent did not widen the application's namespace or move funds.

## Real signed notification, path repair and replay

The owner approved the exact loopback IP `127.0.0.1` and configured all seven
lifecycle signals. The first URL was entered incorrectly as `/notifications`;
the example accepts `/events`. The actual worker retained three HTTP-404 attempts
with sanitized `receiver rejected` status. This was an acceptance configuration
mistake, not a server defect. The owner disabled that endpoint through its
reasoned confirmation dialog and created a replacement for `/events`, then the
receiver restarted with the replacement's private signing secret.

The independent application archived its own account. The actual oxsum worker
sent delivery `ed1be3b2-f117-44d1-b519-a63b57f5fd9c` to replacement endpoint
`463c5d37-4531-4e21-9b36-17a7a723ff47`. The receiver returned **204 on attempt 1**,
verified the signature before parsing, durably retained the original bytes, and
read the current archived entitlement through its scoped credential. The UI
showed Delivered. The old disabled endpoint retained its pending history and
received no new enqueue; disabling did not rewrite prior attempts as success.

A separate explicit local replay used the same original body/delivery identity:
invalid signature **401**, two valid replays **204/204**, with exactly **one**
SQLite inbox row. No raw SQLite store is published. `receiver-state.json` contains
the original event, its SHA-256, processed status and current scoped snapshot;
`receiver-replay.json` records this distinct fixture replay.

## Validation boundary

Five screenshots were visually inspected. JSON secret-field checks, original
artifact hashes and local links passed. This proves the browser configuration and
human handoff plus one actual signed local lifecycle delivery. It does not repeat
or certify the automated example's full checkout/AI/evidence matrix, final exact
candidate J18, production ingress, real PSP/provider, native reference capacity,
restore or independent operator pilots. Those acceptance gates remain open.
