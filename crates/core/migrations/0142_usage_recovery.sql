-- Dedicated reservations protect new-policy money from legacy age-based sweeping.
CREATE TABLE oxsum.usage_recovery (
 operation_id uuid PRIMARY KEY REFERENCES oxsum.operation_billing_terms,
 organization_id uuid NOT NULL REFERENCES oxsum.organizations,
 hold_key text NOT NULL UNIQUE,
 execution_owner text NOT NULL,
 mandate_hash text,
 guardrails jsonb NOT NULL DEFAULT '[]',
 window_secs bigint NOT NULL CHECK(window_secs BETWEEN 1 AND 900),
 route_index bigint,
 attempt_id uuid,
 terminal_observation text,
 terminal_at timestamptz,
 deadline timestamptz,
 observed jsonb NOT NULL DEFAULT '{}',
 claim_token uuid,
 claim_until timestamptz,
 next_lookup_at timestamptz,
 lookup_attempts bigint NOT NULL DEFAULT 0,
 closed_at timestamptz,
 close_reason text,
 waived jsonb NOT NULL DEFAULT '[]',
 supplier_cost_minor bigint,
 CHECK ((terminal_at IS NULL) = (deadline IS NULL))
);
CREATE INDEX usage_recovery_pending ON oxsum.usage_recovery(next_lookup_at) WHERE closed_at IS NULL;
-- Restricted dispatch context never appears in customer exports. No plaintext keys.
CREATE TABLE oxsum.usage_recovery_attempts (
 attempt_id uuid PRIMARY KEY,
 operation_id uuid NOT NULL UNIQUE REFERENCES oxsum.usage_recovery,
 route_index bigint NOT NULL,
 channel_id uuid NOT NULL,
 credential_id uuid NOT NULL,
 base_url text NOT NULL,
 configuration_updated_at timestamptz NOT NULL,
 credential_configuration text NOT NULL,
 destination_policy jsonb NOT NULL,
 supplier_price jsonb NOT NULL,
 experiment jsonb,
 lookup_supported boolean NOT NULL,
 started_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE TRIGGER usage_recovery_attempts_immutable BEFORE UPDATE OR DELETE ON oxsum.usage_recovery_attempts FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
