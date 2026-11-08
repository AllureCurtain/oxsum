-- Published offers own both purchased and promotional quantities.
CREATE TABLE oxsum.bundle_offers (
    bundle_version_id uuid PRIMARY KEY,
    bundle_id uuid NOT NULL,
    business_id uuid NOT NULL REFERENCES oxsum.businesses(business_id),
    version integer NOT NULL CHECK (version > 0),
    terms jsonb NOT NULL,
    published_at timestamptz NOT NULL DEFAULT now(),
    retired_at timestamptz,
    UNIQUE (business_id, bundle_id, version)
);
CREATE FUNCTION oxsum.protect_bundle_offer() RETURNS trigger AS $guard$
BEGIN
    IF TG_OP = 'DELETE' THEN RAISE EXCEPTION 'bundle offer history is retained'; END IF;
    IF (to_jsonb(NEW) - 'retired_at') IS DISTINCT FROM (to_jsonb(OLD) - 'retired_at')
        OR (OLD.retired_at IS NOT NULL AND NEW.retired_at IS DISTINCT FROM OLD.retired_at) THEN
        RAISE EXCEPTION 'published bundle terms are immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER bundle_offers_immutable BEFORE UPDATE OR DELETE ON oxsum.bundle_offers
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_bundle_offer();
CREATE TABLE oxsum.bundle_offer_mutations (
    mutation_id uuid PRIMARY KEY REFERENCES oxsum.business_mutations(mutation_id),
    result jsonb NOT NULL
);
CREATE TRIGGER bundle_offer_mutations_immutable BEFORE UPDATE OR DELETE ON oxsum.bundle_offer_mutations
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
ALTER TABLE oxsum.payment_orders ADD COLUMN bundle_offer jsonb;
CREATE FUNCTION oxsum.validate_order_bundle_offer() RETURNS trigger AS $guard$
DECLARE
    offered oxsum.bundle_offers%ROWTYPE;
BEGIN
    IF NEW.purpose <> 'bundle' THEN
        IF NEW.bundle_offer IS NOT NULL THEN
            RAISE EXCEPTION 'only bundle orders carry a bundle offer';
        END IF;
        RETURN NEW;
    END IF;
    -- Match the application lock order, including old writers after migration.
    PERFORM 1 FROM oxsum.businesses WHERE business_id=NEW.business_id FOR UPDATE;
    SELECT * INTO offered FROM oxsum.bundle_offers
        WHERE business_id=NEW.business_id AND bundle_version_id=NEW.purpose_ref
          AND retired_at IS NULL;
    IF NOT FOUND OR NEW.bundle_offer IS NULL
        OR (NEW.bundle_offer->>'bundleVersionId')::uuid IS DISTINCT FROM offered.bundle_version_id
        OR (NEW.bundle_offer->>'bundleId')::uuid IS DISTINCT FROM offered.bundle_id
        OR (NEW.bundle_offer->>'version')::integer IS DISTINCT FROM offered.version
        OR (NEW.bundle_offer->>'publishedAt')::timestamptz IS DISTINCT FROM offered.published_at
        OR NEW.bundle_offer->>'retiredAt' IS NOT NULL
        OR NEW.bundle_offer->'terms' IS DISTINCT FROM offered.terms
        OR NEW.amount_minor IS DISTINCT FROM (offered.terms->>'purchasedMinor')::bigint
        OR NEW.promo_minor IS DISTINCT FROM (offered.terms->>'promoMinor')::bigint
        OR NEW.display_amount_minor IS DISTINCT FROM (offered.terms->>'displayAmountMinor')::bigint
        OR NEW.display_currency IS DISTINCT FROM offered.terms->>'displayCurrency'
        OR NEW.rate_mantissa::numeric * NEW.amount_minor <> NEW.display_amount_minor::numeric * 10000 * power(10::numeric, NEW.rate_scale) THEN
        RAISE EXCEPTION 'new bundle orders require the exact published offer';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER payment_order_bundle_valid BEFORE INSERT ON oxsum.payment_orders
    FOR EACH ROW EXECUTE FUNCTION oxsum.validate_order_bundle_offer();
CREATE FUNCTION oxsum.protect_order_bundle_offer() RETURNS trigger AS $guard$
BEGIN
    IF NEW.bundle_offer IS DISTINCT FROM OLD.bundle_offer THEN
        RAISE EXCEPTION 'order bundle snapshot is immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER payment_order_bundle_immutable BEFORE UPDATE ON oxsum.payment_orders
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_order_bundle_offer();
-- Legacy orders are not assigned invented offer authority. Fulfilled grants and
-- refunds retain their history; unfulfilled bundles require operator resolution.
