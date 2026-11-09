-- Owner-authorized immutable destinations; scoped runtime never owns their secrets.
ALTER TABLE oxsum.integration_credentials DROP CONSTRAINT integration_credentials_scopes_check;
ALTER TABLE oxsum.integration_credentials ADD CONSTRAINT integration_credentials_scopes_check CHECK (
    cardinality(scopes) BETWEEN 1 AND 12
    AND scopes <@ ARRAY['customersRead','meteringWrite','customersProvision','billingRead','customersLifecycleWrite','customerKeysWrite','entitlementsRead','customerLinkWrite','evidenceRead','checkoutWrite','evidenceWrite','notificationsRead']::text[]
    AND (application_id IS NOT NULL OR NOT (scopes && ARRAY['customersLifecycleWrite','customerKeysWrite','entitlementsRead','customerLinkWrite','evidenceRead','checkoutWrite','evidenceWrite','notificationsRead']::text[]))
);
CREATE TABLE oxsum.application_notification_endpoints (
    endpoint_id uuid PRIMARY KEY,
    application_id uuid NOT NULL REFERENCES oxsum.integration_applications(application_id),
    url text NOT NULL CHECK(octet_length(url) BETWEEN 1 AND 2000),
    destination_policy jsonb NOT NULL,
    events text[] NOT NULL CHECK(cardinality(events) BETWEEN 1 AND 7 AND events <@ ARRAY['customer.changed','key.changed','entitlement.changed','order.changed','statement.changed','evidence.published','balance.changed']::text[]),
    secret_sealed text NOT NULL,
    secret_last4 text NOT NULL,
    enabled boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX application_notification_endpoints_application ON oxsum.application_notification_endpoints(application_id,endpoint_id);
CREATE FUNCTION oxsum.protect_application_notification_endpoint() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='DELETE' OR (to_jsonb(NEW)-'enabled') IS DISTINCT FROM (to_jsonb(OLD)-'enabled') OR (NOT OLD.enabled AND NEW.enabled) THEN
        RAISE EXCEPTION 'notification destination identity is immutable; create a replacement endpoint';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER application_notification_endpoint_identity BEFORE UPDATE OR DELETE ON oxsum.application_notification_endpoints FOR EACH ROW EXECUTE FUNCTION oxsum.protect_application_notification_endpoint();
CREATE TABLE oxsum.application_notification_deliveries (
    delivery_id uuid PRIMARY KEY,
    endpoint_id uuid NOT NULL REFERENCES oxsum.application_notification_endpoints(endpoint_id),
    organization_id uuid NOT NULL REFERENCES oxsum.organizations(organization_id),
    event_type text NOT NULL,
    entity_id uuid NOT NULL,
    body text NOT NULL CHECK(octet_length(body)<=4096),
    status text NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','sending','delivered','failed')),
    attempts integer NOT NULL DEFAULT 0 CHECK(attempts>=0),
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    claimed_at timestamptz,
    response_status integer,
    last_error text,
    created_at timestamptz NOT NULL DEFAULT now(),
    delivered_at timestamptz
);
CREATE INDEX application_notification_deliveries_due ON oxsum.application_notification_deliveries(next_attempt_at,delivery_id) WHERE status IN ('pending','sending');
CREATE INDEX application_notification_deliveries_endpoint ON oxsum.application_notification_deliveries(endpoint_id,delivery_id);
CREATE FUNCTION oxsum.protect_application_notification_body() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='DELETE' OR ROW(NEW.delivery_id,NEW.endpoint_id,NEW.organization_id,NEW.event_type,NEW.entity_id,NEW.body,NEW.created_at) IS DISTINCT FROM ROW(OLD.delivery_id,OLD.endpoint_id,OLD.organization_id,OLD.event_type,OLD.entity_id,OLD.body,OLD.created_at) THEN
        RAISE EXCEPTION 'notification source and bytes are immutable';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER application_notification_body BEFORE UPDATE OR DELETE ON oxsum.application_notification_deliveries FOR EACH ROW EXECUTE FUNCTION oxsum.protect_application_notification_body();

-- Signals never project state or secrets: consumers deduplicate IDs and read current state.
-- Every enqueue participates in the original source transaction, including rollback.
CREATE FUNCTION oxsum.enqueue_application_signal(org uuid, event_name text, entity uuid, entity_kind text) RETURNS void LANGUAGE plpgsql AS $$
DECLARE app uuid; endpoint uuid; delivery uuid;
BEGIN
    SELECT application_id INTO app FROM oxsum.application_customers WHERE organization_id=org;
    IF app IS NULL THEN RETURN; END IF;
    FOR endpoint IN SELECT endpoint_id FROM oxsum.application_notification_endpoints WHERE application_id=app AND enabled AND event_name=ANY(events) ORDER BY endpoint_id LOOP
        delivery:=gen_random_uuid();
        INSERT INTO oxsum.application_notification_deliveries(delivery_id,endpoint_id,organization_id,event_type,entity_id,body)
        VALUES(delivery,endpoint,org,event_name,entity,jsonb_build_object('version',1,'id',delivery,'type',event_name,'applicationId',app,'organizationId',org,'entityId',entity,'entityKind',entity_kind,'action','readCurrentState','createdAt',clock_timestamp())::text);
    END LOOP;
END $$;
CREATE FUNCTION oxsum.application_lifecycle_signal() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE org uuid; entity uuid; event_name text; entity_kind text;
BEGIN
    CASE TG_TABLE_NAME
    WHEN 'application_customers' THEN org:=NEW.organization_id;entity:=org;event_name:='customer.changed';entity_kind:='customer';
    WHEN 'customer_profiles' THEN org:=NEW.organization_id;entity:=org;event_name:='customer.changed';entity_kind:='customer';
    WHEN 'organizations' THEN org:=NEW.organization_id;entity:=org;event_name:='customer.changed';entity_kind:='customer';
    WHEN 'application_customer_keys' THEN org:=NEW.organization_id;entity:=NEW.key_id;event_name:='key.changed';entity_kind:='key';
    WHEN 'api_keys' THEN
        SELECT organization_id INTO org FROM oxsum.application_customer_keys WHERE key_id=NEW.key_id;
        entity:=NEW.key_id;event_name:='key.changed';entity_kind:='key';
    WHEN 'customer_plans' THEN org:=NEW.organization_id;entity:=NEW.customer_plan_id;event_name:='entitlement.changed';entity_kind:='customerPlan';
    WHEN 'plan_periods' THEN
        SELECT organization_id INTO org FROM oxsum.customer_plans WHERE customer_plan_id=NEW.customer_plan_id;
        entity:=NEW.customer_plan_id;event_name:='entitlement.changed';entity_kind:='customerPlan';
    WHEN 'application_checkout_orders' THEN org:=NEW.organization_id;entity:=NEW.order_id;event_name:='order.changed';entity_kind:='order';
    WHEN 'payment_orders' THEN
        SELECT organization_id INTO org FROM oxsum.application_checkout_orders WHERE order_id=NEW.order_id;
        entity:=NEW.order_id;event_name:='order.changed';entity_kind:='order';
        -- Portal funding also changes the shared customer balance, without exposing its order ID.
        IF org IS NULL THEN org:=NEW.organization_id;entity:=org;event_name:='balance.changed';entity_kind:='customer'; END IF;
    WHEN 'refunds','disputes' THEN
        SELECT organization_id INTO org FROM oxsum.application_checkout_orders WHERE order_id=NEW.order_id;
        entity:=NEW.order_id;event_name:='order.changed';entity_kind:='order';
        IF org IS NULL THEN org:=NEW.organization_id;entity:=org;event_name:='balance.changed';entity_kind:='customer'; END IF;
    WHEN 'plan_statements' THEN org:=NEW.organization_id;entity:=NEW.statement_id;event_name:='statement.changed';entity_kind:='statement';
    WHEN 'evidence_bundles' THEN org:=NEW.organization_id;entity:=NEW.bundle_id;event_name:='evidence.published';entity_kind:='evidenceBundle';
    WHEN 'usage_records' THEN
        SELECT organization_id INTO org FROM oxsum.organizations WHERE tenant_id=NEW.tenant_id;
        entity:=org;event_name:='balance.changed';entity_kind:='customer';
    ELSE RAISE EXCEPTION 'unsupported application notification source';
    END CASE;
    IF org IS NOT NULL THEN PERFORM oxsum.enqueue_application_signal(org,event_name,entity,entity_kind); END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER application_customer_signal AFTER INSERT ON oxsum.application_customers FOR EACH ROW EXECUTE FUNCTION oxsum.application_lifecycle_signal();
CREATE TRIGGER application_customer_profile_signal AFTER UPDATE ON oxsum.customer_profiles FOR EACH ROW WHEN (OLD IS DISTINCT FROM NEW) EXECUTE FUNCTION oxsum.application_lifecycle_signal();
CREATE TRIGGER application_customer_organization_signal AFTER UPDATE ON oxsum.organizations FOR EACH ROW WHEN (OLD IS DISTINCT FROM NEW) EXECUTE FUNCTION oxsum.application_lifecycle_signal();
CREATE TRIGGER application_key_create_signal AFTER INSERT ON oxsum.application_customer_keys FOR EACH ROW EXECUTE FUNCTION oxsum.application_lifecycle_signal();
CREATE TRIGGER application_key_update_signal AFTER UPDATE ON oxsum.api_keys FOR EACH ROW WHEN (OLD IS DISTINCT FROM NEW) EXECUTE FUNCTION oxsum.application_lifecycle_signal();
CREATE TRIGGER application_plan_create_signal AFTER INSERT ON oxsum.customer_plans FOR EACH ROW EXECUTE FUNCTION oxsum.application_lifecycle_signal();
CREATE TRIGGER application_plan_update_signal AFTER UPDATE ON oxsum.customer_plans FOR EACH ROW WHEN (OLD IS DISTINCT FROM NEW) EXECUTE FUNCTION oxsum.application_lifecycle_signal();
CREATE TRIGGER application_period_create_signal AFTER INSERT ON oxsum.plan_periods FOR EACH ROW EXECUTE FUNCTION oxsum.application_lifecycle_signal();
CREATE TRIGGER application_period_update_signal AFTER UPDATE ON oxsum.plan_periods FOR EACH ROW WHEN (OLD IS DISTINCT FROM NEW) EXECUTE FUNCTION oxsum.application_lifecycle_signal();
CREATE TRIGGER application_order_create_signal AFTER INSERT ON oxsum.application_checkout_orders FOR EACH ROW EXECUTE FUNCTION oxsum.application_lifecycle_signal();
CREATE TRIGGER application_order_update_signal AFTER UPDATE ON oxsum.payment_orders FOR EACH ROW WHEN (OLD IS DISTINCT FROM NEW) EXECUTE FUNCTION oxsum.application_lifecycle_signal();
CREATE TRIGGER application_refund_create_signal AFTER INSERT ON oxsum.refunds FOR EACH ROW EXECUTE FUNCTION oxsum.application_lifecycle_signal();
CREATE TRIGGER application_refund_update_signal AFTER UPDATE ON oxsum.refunds FOR EACH ROW WHEN (OLD IS DISTINCT FROM NEW) EXECUTE FUNCTION oxsum.application_lifecycle_signal();
CREATE TRIGGER application_dispute_create_signal AFTER INSERT ON oxsum.disputes FOR EACH ROW EXECUTE FUNCTION oxsum.application_lifecycle_signal();
CREATE TRIGGER application_dispute_update_signal AFTER UPDATE ON oxsum.disputes FOR EACH ROW WHEN (OLD IS DISTINCT FROM NEW) EXECUTE FUNCTION oxsum.application_lifecycle_signal();
CREATE TRIGGER application_statement_create_signal AFTER INSERT ON oxsum.plan_statements FOR EACH ROW EXECUTE FUNCTION oxsum.application_lifecycle_signal();
CREATE TRIGGER application_statement_update_signal AFTER UPDATE ON oxsum.plan_statements FOR EACH ROW WHEN (OLD IS DISTINCT FROM NEW) EXECUTE FUNCTION oxsum.application_lifecycle_signal();
CREATE TRIGGER application_evidence_signal AFTER INSERT ON oxsum.evidence_bundles FOR EACH ROW EXECUTE FUNCTION oxsum.application_lifecycle_signal();
CREATE TRIGGER application_usage_create_signal AFTER INSERT ON oxsum.usage_records FOR EACH ROW EXECUTE FUNCTION oxsum.application_lifecycle_signal();
CREATE TRIGGER application_usage_update_signal AFTER UPDATE ON oxsum.usage_records FOR EACH ROW WHEN (OLD.status IS DISTINCT FROM NEW.status) EXECUTE FUNCTION oxsum.application_lifecycle_signal();
