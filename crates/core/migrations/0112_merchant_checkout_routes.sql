-- Explicit owner checkout selection is independent of legacy callback bootstrap.
CREATE TABLE oxsum.merchant_checkout_routes (
    business_id uuid NOT NULL REFERENCES oxsum.businesses(business_id),
    rail text NOT NULL CHECK (rail IN ('hyperswitch','stripe','airwallex')),
    revision bigint NOT NULL CHECK (revision > 0),
    merchant_id uuid REFERENCES oxsum.merchant_accounts(merchant_id),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(business_id,rail)
);
CREATE TABLE oxsum.merchant_checkout_route_results (
    mutation_id uuid PRIMARY KEY REFERENCES oxsum.business_mutations(mutation_id),
    route jsonb NOT NULL
);
CREATE TRIGGER merchant_checkout_route_results_immutable BEFORE UPDATE OR DELETE
    ON oxsum.merchant_checkout_route_results FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
