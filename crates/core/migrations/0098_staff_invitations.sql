-- Staff invitations: an email-bound, expiring grant of a business staff role.
-- The role is fixed at creation and can never be owner; the invited account
-- joins business_staff on acceptance, not a customer membership.
ALTER TABLE oxsum.invitations
    DROP CONSTRAINT invitation_target,
    ADD COLUMN staff_role text,
    ADD CONSTRAINT invitation_target CHECK (
        (kind='team' AND organization_id IS NOT NULL AND pending_customer_id IS NULL
            AND customer_name IS NULL AND external_id IS NULL AND staff_role IS NULL)
        OR (kind='customer' AND pending_customer_id IS NOT NULL AND intended_email IS NOT NULL
            AND customer_name IS NOT NULL AND external_id IS NOT NULL
            AND staff_role IS NULL
            AND (organization_id IS NULL OR organization_id=pending_customer_id))
        OR (kind='staff' AND organization_id IS NULL AND pending_customer_id IS NULL
            AND intended_email IS NOT NULL AND customer_name IS NULL AND external_id IS NULL
            AND staff_role IN ('admin','finance','support','viewer'))
    );
ALTER TABLE oxsum.invitations
    DROP CONSTRAINT invitations_kind_check,
    ADD CONSTRAINT invitations_kind_check CHECK (kind IN ('team','customer','staff'));

-- Delivery facts only. The one SMTP attempt is claimed before transmission so
-- a replayed create never resends; plaintext is never stored for retry.
CREATE TABLE oxsum.staff_invitation_mail (
    invitation_id uuid PRIMARY KEY REFERENCES oxsum.invitations (invitation_id) ON DELETE RESTRICT,
    attempted_at timestamptz,
    submitted_at timestamptz
);
