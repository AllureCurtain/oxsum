# Staff invitation browser prerequisite repair

Issue: [#947](https://github.com/AllureCurtain/oxsum/issues/947).
Observed during [#946](https://github.com/AllureCurtain/oxsum/issues/946) isolated
development browser acceptance on source `98238b38ef3708000233aebc606ad286f701be0f`.

A fresh owner created a finance invitation through the portal and submitted it
to the local SMTP sink. Opening the actual mailed link returned HTTP 500 from
the public preview, leaving the consent button disabled. The database diagnostic
was an unexpected null `organization_id`. Staff invitations deliberately have
no customer organization; the preview also failed to select their business name.

The repair uses the existing documented nil UUID for staff and reads the
inviting business name through its business ID. Customer and team previews keep
their own identity path. Token liveness and role validation remain required.
The browser distinguishes business staff from customer membership in the offer,
registration action and acceptance result. Authenticated staff acceptance
continues to the Business console.

## Validation

- Added the missing public-preview step to the real PostgreSQL/local SMTP HTTP
  journey before redemption. On the unchanged baseline it failed with HTTP 500
  instead of 200; after the repair it returns the nil UUID, business name and
  finance role without the token. Accepted and revoked previews remain 404.
- All four grantable roles pass public preview and existing-user acceptance;
  the accepted token no longer previews.
- The focused customer/team/staff invitation selection passed **28 tests**, zero
  skipped, including delivery replay, wrong authority, consent, upgrade and
  concurrent redemption coverage.
- Contract code generation and the TypeScript/production frontend build passed.
- Complete workspace: **2,191 passed**, zero failed, six existing opt-ins skipped
  (1,977.571 seconds). Strict core/server all-target Clippy passed (6m55s).
- Actual browser retry used source `013bc8cb`, binary SHA-256
  `d90c5e178ded6b8e3fe5d9045736f739329a7b57c733907764f020d444f09b37`,
  against the same isolated database and original mailed invitations. A different
  signed-in email was refused. Finance and Support recipients registered, saw
  the staff-specific consent/result and logged into their correct capabilities.
  Finance had no secret editor; Support had no payments/reports/staff management.
  Revoking Support through the owner portal removed authority; the original
  accepted invitation no longer previewed or enabled acceptance. No customer
  organization, key or funds were returned by either staff registration.
- Fast PR CI `42741717964` passed. Independent approval and protected integration
  remain. This is development repair evidence, not final candidate certification.
