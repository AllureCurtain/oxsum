-- Existing authorized periods retain their history. No unsafe pending order
-- receives an invented target period or sold-fee assertion on upgrade.
ALTER TABLE oxsum.payment_orders ADD COLUMN plan_payment jsonb;
CREATE UNIQUE INDEX payment_orders_plan_period_live
    ON oxsum.payment_orders (purpose_ref, ((plan_payment->>'periodIndex')::integer))
    WHERE purpose='plan_period' AND plan_payment IS NOT NULL
      AND status NOT IN ('failed','expired');
CREATE FUNCTION oxsum.protect_order_plan_payment() RETURNS trigger AS $guard$
BEGIN
    IF NEW.plan_payment IS DISTINCT FROM OLD.plan_payment THEN
        RAISE EXCEPTION 'plan payment intent is immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER payment_order_plan_immutable BEFORE UPDATE ON oxsum.payment_orders
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_order_plan_payment();
CREATE FUNCTION oxsum.validate_order_plan_payment() RETURNS trigger AS $guard$
DECLARE
    cp oxsum.customer_plans%ROWTYPE;
    sold oxsum.sold_plans%ROWTYPE;
    collection oxsum.collection_terms%ROWTYPE;
    previous oxsum.plan_periods%ROWTYPE;
    expected_index integer;
    expected_start timestamptz;
    expected_end timestamptz;
    next_month timestamp;
    anchor_day integer;
BEGIN
    IF NEW.purpose <> 'plan_period' THEN
        IF NEW.plan_payment IS NOT NULL THEN RAISE EXCEPTION 'only plan orders carry period intent'; END IF;
        RETURN NEW;
    END IF;
    -- Match the application's business -> plan lock order, including old writers.
    PERFORM 1 FROM oxsum.businesses WHERE business_id=NEW.business_id FOR UPDATE;
    SELECT * INTO cp FROM oxsum.customer_plans
        WHERE customer_plan_id=NEW.purpose_ref AND organization_id=NEW.organization_id FOR UPDATE;
    IF NOT FOUND OR cp.state NOT IN ('pending_payment','active','renewal_pending','past_due')
        OR cp.cancel_at_period_end OR cp.scheduled_change IS NOT NULL THEN
        RAISE EXCEPTION 'plan assignment is not payable';
    END IF;
    SELECT * INTO sold FROM oxsum.sold_plans
        WHERE plan_version_id=cp.plan_version_id AND business_id=NEW.business_id;
    IF NOT FOUND THEN RAISE EXCEPTION 'plan does not belong to this business'; END IF;
    SELECT * INTO collection FROM oxsum.collection_terms WHERE business_id=NEW.business_id;
    IF NOT FOUND THEN RAISE EXCEPTION 'plan collection is not configured'; END IF;
    IF cp.state='pending_payment' THEN
        expected_index := 0;
    ELSE
        SELECT * INTO previous FROM oxsum.plan_periods WHERE customer_plan_id=cp.customer_plan_id
            ORDER BY period_index DESC LIMIT 1;
        IF NOT FOUND OR previous.period_end > clock_timestamp() THEN
            RAISE EXCEPTION 'manual renewal is not yet due';
        END IF;
        expected_index := previous.period_index + 1;
        expected_start := previous.period_end;
        next_month := date_trunc('month', expected_start AT TIME ZONE 'UTC') + interval '1 month';
        IF sold.period_kind='anniversary' THEN
            anchor_day := EXTRACT(DAY FROM COALESCE(cp.activated_at,expected_start) AT TIME ZONE 'UTC');
            next_month := next_month + (LEAST(anchor_day,
                EXTRACT(DAY FROM next_month + interval '1 month - 1 day')::integer)-1) * interval '1 day';
        END IF;
        expected_end := next_month AT TIME ZONE 'UTC';
    END IF;
    IF NEW.plan_payment IS NULL OR jsonb_typeof(NEW.plan_payment) IS DISTINCT FROM 'object'
        OR NOT (NEW.plan_payment ?& ARRAY['customerPlanId','planVersionId','periodIndex','periodStart',
            'periodEnd','feeMinor','collectionRevision','displayCurrency','displayAmountMinor','rateMantissa','rateScale'])
        OR (NEW.plan_payment - ARRAY['customerPlanId','planVersionId','periodIndex','periodStart',
            'periodEnd','feeMinor','collectionRevision','displayCurrency','displayAmountMinor','rateMantissa','rateScale']) <> '{}'::jsonb
        OR (NEW.plan_payment->>'customerPlanId')::uuid IS DISTINCT FROM NEW.purpose_ref
        OR (NEW.plan_payment->>'planVersionId')::uuid IS DISTINCT FROM cp.plan_version_id
        OR (NEW.plan_payment->>'feeMinor')::bigint IS DISTINCT FROM sold.price_minor
        OR NEW.amount_minor IS DISTINCT FROM sold.price_minor
        OR sold.price_minor NOT BETWEEN 1 AND 9007199254740991
        OR NEW.promo_minor<>0 OR NEW.currency<>'USD'
        OR NEW.display_currency IS DISTINCT FROM sold.currency
        OR NEW.display_currency IS DISTINCT FROM collection.currency
        OR NEW.plan_payment->>'displayCurrency' IS DISTINCT FROM sold.currency
        OR (NEW.plan_payment->>'displayAmountMinor')::bigint IS DISTINCT FROM NEW.display_amount_minor
        OR NEW.display_amount_minor NOT BETWEEN 1 AND 9007199254740991
        OR NEW.display_amount_minor IS DISTINCT FROM CEIL(NEW.amount_minor::numeric * NEW.rate_mantissa / (10000 * power(10::numeric,NEW.rate_scale)))::bigint
        OR (NEW.plan_payment->>'rateMantissa')::bigint IS DISTINCT FROM NEW.rate_mantissa
        OR (NEW.plan_payment->>'rateScale')::integer IS DISTINCT FROM NEW.rate_scale
        OR NEW.rate_scale NOT BETWEEN 0 AND 8
        OR NEW.rate_mantissa::numeric / power(10::numeric,NEW.rate_scale) IS DISTINCT FROM collection.rate_per_credit::numeric
        OR (NEW.plan_payment->>'collectionRevision')::bigint IS DISTINCT FROM collection.revision
        OR (NEW.plan_payment->>'periodIndex')::integer IS DISTINCT FROM expected_index
        OR (NEW.plan_payment->>'periodStart')::timestamptz IS DISTINCT FROM expected_start
        OR (NEW.plan_payment->>'periodEnd')::timestamptz IS DISTINCT FROM expected_end THEN
        RAISE EXCEPTION 'plan order requires an exact customer and sold period quote';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER payment_order_plan_valid BEFORE INSERT ON oxsum.payment_orders
    FOR EACH ROW EXECUTE FUNCTION oxsum.validate_order_plan_payment();

-- Partial prepayments have explicit source-linked payment lines. They reduce
-- outstanding through paid_minor, never the document's gross charge total.
CREATE OR REPLACE FUNCTION oxsum.protect_plan_statement() RETURNS trigger AS $guard$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'statement history is retained'; END IF;
    IF NEW.statement_id IS DISTINCT FROM OLD.statement_id
        OR NEW.customer_plan_id IS DISTINCT FROM OLD.customer_plan_id
        OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.business_id IS DISTINCT FROM OLD.business_id
        OR NEW.period_index IS DISTINCT FROM OLD.period_index
        OR NEW.journal_id IS DISTINCT FROM OLD.journal_id
        OR NEW.currency IS DISTINCT FROM OLD.currency
        OR NEW.replaces_statement_id IS DISTINCT FROM OLD.replaces_statement_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'statement identity and terms are immutable';
    END IF;
    -- `total_minor` tracks charge/credit lines, excluding payment allocations.
    -- A refund or
    -- dispute correction appends a line then lands the figure — the trigger
    -- sees the committed lines, so writers append before they update.
    IF NEW.total_minor IS DISTINCT FROM OLD.total_minor THEN
        IF OLD.state = 'void' THEN
            RAISE EXCEPTION 'a settled statement cannot be re-priced';
        END IF;
        IF NEW.total_minor IS DISTINCT FROM (
            SELECT COALESCE(SUM(amount_minor),0)::bigint
            FROM oxsum.plan_statement_lines WHERE statement_id = NEW.statement_id
                AND kind <> 'payment_applied'
        ) THEN
            RAISE EXCEPTION 'total_minor must equal the statement charge lines sum';
        END IF;
    END IF;
    IF NEW.state IS DISTINCT FROM OLD.state THEN
        IF NOT (
            (OLD.state='open'     AND NEW.state IN ('issued','void')) OR
            (OLD.state='issued'   AND NEW.state IN ('paid','past_due','void')) OR
            (OLD.state='past_due' AND NEW.state IN ('paid','issued','void')) OR
            (OLD.state='paid'     AND NEW.state IN ('void'))
        ) THEN
            RAISE EXCEPTION 'invalid statement state transition % -> %', OLD.state, NEW.state;
        END IF;
    END IF;
    IF OLD.state='void' THEN
        RAISE EXCEPTION 'a void statement is immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
