# Staff invitations and role lifecycle development evidence

Issue: [#734](https://github.com/AllureCurtain/oxsum/issues/734) (work unit N08). Based on main `0314cbd`. Development evidence, not a merged candidate; actual browser and external mail delivery remain pending.

## Implemented behavior

Business staff onboarding no longer requires the owner to learn a new user's internal UUID. `POST /api/v1/business/staff-invitations` (StaffManage — owner only) creates an email-bound, seven-day, single-use invitation carrying an explicit staff role, and `GET` on the same path lists this business's staff invitations without exposing token material. `POST .../revoke` withdraws a pending one.

The grantable role set is `admin`, `finance`, `support` and `viewer`. `owner` is refused in the contract and in core — ownership still moves only through the existing ownership-transfer path, never through an invitation. The inviter can offer at most a role they may grant; because only `owner` holds `StaffManage`, every staff invitation today originates from the owner.

Acceptance reuses the existing consent primitives. `POST /api/v1/invitations/preview` reports the pending staff role; `POST /api/v1/invitations/accept` joins an already-registered recipient whose session email matches the bound address into `oxsum.business_staff` at the invited role — not into a customer membership, with no customer funds or account access. `POST /api/v1/invitations/redeem` registers a brand-new recipient who then lands the same staff seat. In both cases the recipient must act through their own session on the email the invitation was bound to; a forwarded token cannot grant authority to a different account.

Revocation deletes the staff seat so the role's capabilities stop resolving, and a replayed acceptance cannot restore a removed seat — the second acceptance reads the already-committed state and reports the standing outcome. An invitation that named a role is refused if it would overwrite an existing `owner` seat.

Migration `0098` adds `kind='staff'` plus a `staff_role` column to `oxsum.invitations`, widens the `invitation_target` check to admit a staff row, keeps the trigger that snapshots customer profiles untouched — a staff invite creates no customer profile — and adds `oxsum.staff_invitation_mail`, the at-most-once claim row that records the single allowed SMTP attempt so a replayed create never resends. The legacy `POST /api/v1/business/staff` UUID appointment endpoint is retained with its original restricted authority and audit for compatibility.

A deliberate contract trade-off: `InvitationPreview`/`InvitationAcceptance` keep `role` required as a membership role for the customer/team paths, so a staff invitation still serializes `role` (the placeholder `member`) beside its real grant in `staffRole`. Making `role` nullable would be a breaking change to a required field every existing consumer reads; the authoritative staff grant lives in `staffRole`, which the portal already prefers (`staffRole ?? role`). The placeholder is documented on the schema, not a silent claim that the recipient becomes a member.

## Validation

`cargo nextest run -p oxsum-core --test staff_invitations --test invitation_consent -p oxsum-server --test staff_invitation_delivery` — 14 passed, 0 failed. `scripts/fmt.sh` clean; `cargo clippy --workspace --all-targets` clean; `npm --prefix web run build` clean (`tsc -b` + `vite build`). Contract `crates/server/openapi.yaml` 0.122.0 first.

## Remaining gates

Actual browser walkthrough of the staff invitation flow and external mail delivery are pending the task browser's recovery. No live provider inference, credential authentication or real payment was attempted.
