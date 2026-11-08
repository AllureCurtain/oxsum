# Customer invitation and registration controls development evidence

Issue: [#733](https://github.com/AllureCurtain/oxsum/issues/733) (work unit N07). Based on main `0314cbd`. Development evidence, not a merged candidate; actual browser and external mail delivery remain pending.

## Implemented behavior

The business owner now has a safe customer-invitation history at `GET /api/v1/business/customer-invitations` (CustomersRead). The list is scoped to this business's customer invitations only — never team invitations, never token hashes or plaintext. The page also reports `mailConfigured` (the server holds an SMTP configuration; external delivery is unverified) and the saved `registrationMode`.

Creation keeps the existing seven-day, email-bound, single-use consent mechanism. `sendEmail` on the request explicitly asks for one SMTP submission to the bound recipient. `InvitationDeliveryState` records only submission facts: `notRequested`, `notAttempted`, `submitted`, `unavailable` (no mailer configured) and `unknown` (interrupted, timed-out or rejected transport). `submitted` means the configured transport accepted the message — never that the recipient received it.

The one attempt is claimed in `oxsum.customer_invitation_mail` before transmission, so an HTTP replay of the same idempotent request returns the standing invitation with `token: null` and never resends. An ambiguous outcome stays `unknown`; the owner recovers a lost link by revoking and creating a fresh invitation. Copy-link creation (`sendEmail` false) works with no mailer at all.

Registration enrollment grants no staff role and no funds: the customer starts with a zero wallet and cannot operate the business. The owner-facing registration policy (`/api/v1/business/registration`) and revocation remain business-authority writes; reads of the invitation history and policy require the current business session.

## Validation

- New `crates/server/tests/customer_invitation_delivery.rs` drives a real local SMTP stub: one submission enrolls a zero-fund, non-staff customer; the bound token registers once; a wrong-email redeem is not-found; a replay does not resend; missing and refused transports keep copy links and never auto-retry; registration mode and revoke are owner-only.
- New `customer_invitation_history_is_scoped_and_never_contains_a_secret` in `crates/core/tests/invitation_consent.rs` asserts the page hides token material and stays business-scoped.
- The mail-claim transaction `invitation_mail_claim_survives_replay_and_crash_without_a_second_send` proves at-most-once submission.
- Adjacent suites: 42 invitation/business/email/staff PostgreSQL tests passed; strict workspace Clippy clean; frontend typecheck/build passed.

## Remaining gates

Actual browser walkthrough of the portal invitations page is pending the task browser's recovery. No external mail delivery, no live provider inference, no real payment; the shared budget remains 0/5 attempts and CNY 0.
