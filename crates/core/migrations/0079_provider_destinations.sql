-- Legacy channels default to public policy at use time. Internal access must
-- be explicitly approved; migration never guesses network authorization.
CREATE TABLE oxsum.provider_connection_checks (
    mutation_id uuid PRIMARY KEY REFERENCES oxsum.business_mutations(mutation_id),
    channel_id uuid NOT NULL REFERENCES oxsum.channels(channel_id),
    state text NOT NULL DEFAULT 'pending' CHECK (state IN
        ('pending','reachable','authenticationFailed','httpError','blocked','unreachable')),
    http_status integer CHECK (http_status BETWEEN 100 AND 599),
    created_at timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz
);
CREATE FUNCTION oxsum.protect_provider_check() RETURNS trigger AS $guard$
BEGIN
    IF TG_OP='DELETE' OR OLD.state<>'pending'
        OR NEW.mutation_id IS DISTINCT FROM OLD.mutation_id
        OR NEW.channel_id IS DISTINCT FROM OLD.channel_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'provider connection check history is immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER provider_checks_immutable BEFORE UPDATE OR DELETE
    ON oxsum.provider_connection_checks FOR EACH ROW EXECUTE FUNCTION oxsum.protect_provider_check();

-- New submissions store their resolved connection and sealed credential.
-- Old tasks can only capture the directory as it exists at this upgrade;
-- pre-upgrade endpoint/credential history did not exist and is not invented.
ALTER TABLE oxsum.tasks ADD COLUMN provider_endpoint jsonb;
UPDATE oxsum.tasks t SET provider_endpoint=jsonb_build_object(
    'baseUrl',c.base_url,'protocol',c.protocol,
    'destinationPolicy',COALESCE(c.settings->'destinationPolicy','{"network":"public","allowedIps":[]}'::jsonb),
    'apiKeySealed',(SELECT cr.api_key_sealed FROM oxsum.channel_credentials cr
       WHERE cr.channel_id=t.channel_id AND cr.status='active' ORDER BY cr.position LIMIT 1))
FROM oxsum.channels c WHERE t.channel_id=c.channel_id;
CREATE FUNCTION oxsum.protect_task_endpoint() RETURNS trigger AS $guard$
BEGIN
    IF NEW.provider_endpoint IS DISTINCT FROM OLD.provider_endpoint THEN
        RAISE EXCEPTION 'task provider endpoint is immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER task_endpoint_immutable BEFORE UPDATE ON oxsum.tasks
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_task_endpoint();
