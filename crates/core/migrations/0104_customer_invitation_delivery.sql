-- Keep only delivery facts. Tokens remain hashed and are never stored for retry.
-- Claim the one SMTP attempt before transmission; an interrupted/failed attempt
-- stays unknown and cannot be replayed into a second outbound message.
ALTER TABLE oxsum.invitations
    ADD COLUMN mail_state text NOT NULL DEFAULT 'not_requested'
        CHECK (mail_state IN ('not_requested','not_attempted','unknown','submitted','unavailable')),
    ADD COLUMN mail_attempted_at timestamptz,
    ADD COLUMN mail_submitted_at timestamptz;

CREATE FUNCTION oxsum.protect_invitation_delivery() RETURNS trigger AS $guard$
BEGIN
    IF NEW.mail_state IS DISTINCT FROM OLD.mail_state AND NOT (
        (OLD.mail_state='not_attempted' AND NEW.mail_state IN ('unknown','unavailable')) OR
        (OLD.mail_state='unknown' AND NEW.mail_state='submitted')
    ) THEN
        RAISE EXCEPTION 'invitation mail is submitted at most once';
    END IF;
    IF OLD.mail_attempted_at IS NOT NULL AND NEW.mail_attempted_at IS DISTINCT FROM OLD.mail_attempted_at THEN
        RAISE EXCEPTION 'invitation mail attempt is immutable';
    END IF;
    IF OLD.mail_submitted_at IS NOT NULL AND NEW.mail_submitted_at IS DISTINCT FROM OLD.mail_submitted_at THEN
        RAISE EXCEPTION 'invitation mail submission is immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER invitation_delivery_protected BEFORE UPDATE ON oxsum.invitations
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_invitation_delivery();
