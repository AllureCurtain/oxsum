-- Issue #158 (roadmap P7-1): per-organization commercial terms, in two halves.
--
-- `tier_profiles` names a capability package — an organization-wide
-- requests-per-minute allowance and a model allowlist — that `organizations.tier`
-- assigns. Per docs/decisions.md a tier is never a pricing input; it gates
-- admission only. An organization without a tier is unconstrained, as before.
CREATE TABLE oxsum.tier_profiles (
    -- A slug; `organizations.tier` references it by name.
    name                 text        PRIMARY KEY,
    -- One rolling-minute allowance shared by every key of the assigned
    -- organizations. NULL uncaps.
    requests_per_minute  int         CHECK (requests_per_minute > 0),
    -- The models the tier may call; NULL means everything the deployment
    -- serves. A JSON array of strings.
    model_allowlist      jsonb       CHECK (model_allowlist IS NULL OR jsonb_typeof(model_allowlist) = 'array'),
    created_at           timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE oxsum.organizations
    -- RESTRICT: retiring a tier that organizations still carry is refused, so a
    -- deleted package never silently uncaps its members.
    ADD COLUMN tier text REFERENCES oxsum.tier_profiles (name) ON DELETE RESTRICT;

-- `pricing_discounts` is the second half: organization- and/or model-scoped
-- percents a settlement applies after the priced sum, inside a validity window.
-- Several matching rows never stack — the single most favorable applies — and
-- the applied percent is snapshotted into the settlement description, so a row
-- changed or ended here rewrites no bill.
CREATE TABLE oxsum.pricing_discounts (
    discount_id      uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The write convention's key: a retry with the same fields reads back the
    -- row it created; the same key under different fields conflicts.
    idempotency_key  text        NOT NULL UNIQUE,
    -- NULL scopes the row to every organization / every model — a promotion can
    -- be platform-wide.
    organization_id  uuid        REFERENCES oxsum.organizations (organization_id) ON DELETE RESTRICT,
    model            text,
    -- The percent taken off the priced sum; 100 settles at zero — a discount,
    -- not an unpriced anomaly.
    percent          int         NOT NULL CHECK (percent BETWEEN 1 AND 100),
    -- Why the discount exists, in the operator's own words — the label an audit
    -- of a discounted bill looks for.
    label            text,
    valid_from       timestamptz NOT NULL DEFAULT now(),
    -- NULL runs until ended. A turn qualifies when it starts inside the window.
    valid_until      timestamptz,
    created_at       timestamptz NOT NULL DEFAULT now(),
    CHECK (valid_until IS NULL OR valid_until > valid_from)
);
