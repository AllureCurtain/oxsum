-- Sale terms are independent of provider routes and supplier acquisition costs.
CREATE TABLE oxsum.sale_books (
    business_id uuid NOT NULL REFERENCES oxsum.businesses,
    book_id uuid PRIMARY KEY,
    name text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (business_id, book_id),
    UNIQUE (business_id, name)
);
-- Draft saves append revisions. Publication never mutates the source draft.
CREATE TABLE oxsum.sale_drafts (
    book_id uuid NOT NULL REFERENCES oxsum.sale_books,
    revision bigint NOT NULL CHECK (revision > 0),
    models jsonb NOT NULL CHECK (jsonb_typeof(models) = 'object'),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (book_id, revision)
);
CREATE TABLE oxsum.sale_versions (
    business_id uuid NOT NULL,
    book_id uuid NOT NULL,
    version bigint NOT NULL CHECK (version > 0),
    draft_revision bigint NOT NULL,
    published_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (book_id, version),
    UNIQUE (business_id, book_id, version),
    UNIQUE (book_id, draft_revision),
    FOREIGN KEY (business_id, book_id) REFERENCES oxsum.sale_books (business_id, book_id),
    FOREIGN KEY (book_id, draft_revision) REFERENCES oxsum.sale_drafts (book_id, revision)
);
CREATE TABLE oxsum.sale_schedules (
    business_id uuid NOT NULL REFERENCES oxsum.businesses,
    schedule_id uuid PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('default', 'assigned', 'override')),
    organization_id uuid,
    model text,
    book_id uuid,
    version bigint,
    terms jsonb,
    effective_from timestamptz NOT NULL,
    scheduled_until timestamptz,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK (scheduled_until IS NULL OR scheduled_until > effective_from),
    CHECK ((kind = 'default' AND organization_id IS NULL AND model IS NULL
             AND book_id IS NOT NULL AND version IS NOT NULL AND terms IS NULL)
        OR (kind = 'assigned' AND organization_id IS NOT NULL AND model IS NULL
             AND book_id IS NOT NULL AND version IS NOT NULL AND terms IS NULL)
        OR (kind = 'override' AND organization_id IS NOT NULL AND model IS NOT NULL
             AND length(btrim(model)) BETWEEN 1 AND 200 AND book_id IS NULL
             AND version IS NULL AND terms IS NOT NULL AND jsonb_typeof(terms) = 'object')),
    FOREIGN KEY (business_id, organization_id) REFERENCES oxsum.customer_profiles (business_id, organization_id),
    FOREIGN KEY (business_id, book_id, version) REFERENCES oxsum.sale_versions (business_id, book_id, version)
);
CREATE INDEX sale_schedules_resolution ON oxsum.sale_schedules (business_id, kind, organization_id, model, effective_from);
CREATE INDEX sale_schedules_book ON oxsum.sale_schedules (book_id, effective_from);
-- Ending an open-ended schedule appends a fact, preserving its original window.
CREATE TABLE oxsum.sale_schedule_ends (
    schedule_id uuid PRIMARY KEY REFERENCES oxsum.sale_schedules,
    effective_until timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE TABLE oxsum.sale_mutation_results (
    mutation_id uuid PRIMARY KEY REFERENCES oxsum.business_mutations,
    resource_id uuid NOT NULL,
    revision bigint
);
CREATE TRIGGER sale_books_immutable BEFORE UPDATE OR DELETE ON oxsum.sale_books
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
CREATE TRIGGER sale_drafts_immutable BEFORE UPDATE OR DELETE ON oxsum.sale_drafts
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
CREATE TRIGGER sale_versions_immutable BEFORE UPDATE OR DELETE ON oxsum.sale_versions
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
CREATE TRIGGER sale_schedules_immutable BEFORE UPDATE OR DELETE ON oxsum.sale_schedules
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
CREATE TRIGGER sale_schedule_ends_immutable BEFORE UPDATE OR DELETE ON oxsum.sale_schedule_ends
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
CREATE TRIGGER sale_mutation_results_immutable BEFORE UPDATE OR DELETE ON oxsum.sale_mutation_results
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
