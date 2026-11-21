CREATE TABLE oxsum.provider_model_discoveries (
    mutation_id uuid PRIMARY KEY REFERENCES oxsum.business_mutations(mutation_id),
    channel_id uuid NOT NULL REFERENCES oxsum.channels(channel_id),
    state text NOT NULL DEFAULT 'pending' CHECK (state IN
        ('pending','listed','unsupported','authenticationFailed','httpError','invalidCatalog','blocked','unreachable')),
    http_status integer CHECK (http_status BETWEEN 100 AND 599),
    models text[] NOT NULL DEFAULT '{}' CHECK (cardinality(models) <= 500),
    partial boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz,
    CHECK (state = 'listed' OR (cardinality(models) = 0 AND NOT partial)),
    CHECK ((state = 'pending') = (completed_at IS NULL))
);
CREATE TRIGGER provider_model_discoveries_immutable BEFORE UPDATE OR DELETE
    ON oxsum.provider_model_discoveries FOR EACH ROW EXECUTE FUNCTION oxsum.protect_provider_check();
