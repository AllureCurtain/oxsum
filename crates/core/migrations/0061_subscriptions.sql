-- Subscription quota plans (issue #496): a fixed-period entitlement that lives
-- outside the ledger and refreshes on a cadence, beside the prepaid wallet.
--
-- `oxsum.subscription_plans` is the operator-managed catalogue: what a plan is
-- called, how much quota each period carries (in credits, on the same minor-unit
-- axis the wallet prices in), how often the period resets and whether an
-- exhausted plan may overflow into the wallet. `price_minor` is catalogue
-- information for the admin surface; charging for the plan itself rides the
-- deposit rails, not this table.
--
-- `oxsum.org_subscriptions` is one organization's link to a plan — at most one
-- `active` row per organization. `current_period_start`/`current_period_end`
-- bound the window `consumed_minor` counts inside; rollover is the lazy
-- transition that sees `current_period_end <= now()` and opens the next window,
-- plus the eager pass the `subscription-rollovers` job runs so a quiet
-- organization's reset does not wait for its next request.
--
-- `oxsum.subscription_periods` is the audit row every boundary writes: the
-- period's bounds, the quota it carried and what it consumed. A month's burn is
-- provable against the period's quota without trusting a counter on the live
-- row — the row the rollover superseded stays as the record.
--
-- `oxsum.subscription_draws` is the idempotency side of hold-time debiting:
-- `(hold_key)` names the freeze that drew, `drawn_minor` what the subscription
-- covered of it. Settle reads the row back to release the reserved part and
-- charge the consumed part — the draw table, not a ledger posting, is where a
-- subscription freeze is recorded, because quota is not ledger money.
--
-- The draw rows the wallet posts are outside the ledger: a subscription is an
-- entitlement, not a balance, so its spend is a counter the row's CHECK keeps
-- inside the period's quota. The settled charge's description names
-- `subscription:<id>:period:<id>` so a bill still proves which entitlement
-- covered it.

CREATE TABLE oxsum.subscription_plans (
    plan_id             uuid        PRIMARY KEY,
    name                text        NOT NULL UNIQUE,
    -- Quota per period in credit minor units — the same axis the wallet prices
    -- in, so a plan's burn reuses the money arithmetic rather than a second
    -- currency. NULL means unmetered.
    quota_minor         bigint      CHECK (quota_minor IS NULL OR quota_minor > 0),
    -- How often `consumed_minor` resets: daily/weekly/monthly align to the UTC
    -- boundary of the same name; `never` keeps one running counter for the
    -- subscription's whole life.
    reset_period        text        NOT NULL CHECK (reset_period IN ('daily', 'weekly', 'monthly', 'never')),
    -- When an exhausted plan may fall through to the wallet. Snapshotted onto
    -- the org_subscriptions row at bind time so a plan edit changes only
    -- subscriptions bound after it.
    allow_wallet_overflow boolean   NOT NULL DEFAULT true,
    -- Catalogue price in minor units — what the plan is sold for, when it is
    -- not free. Billing for it rides the deposit rails; the column is the
    -- admin surface's answer to "what does this plan cost".
    price_minor         bigint      CHECK (price_minor IS NULL OR price_minor >= 0),
    -- The Stripe `price_…` a checkout session names, when the plan sells on
    -- that rail. Free-form: the value Stripe assigns is theirs to change.
    stripe_price_id     text,
    -- Whether new subscriptions may bind to the plan. An ended plan keeps its
    -- row — the periods that ran under it stay readable — it only stops
    -- accepting new binds.
    enabled             boolean     NOT NULL DEFAULT true,
    sort_order          integer     NOT NULL DEFAULT 0,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE oxsum.org_subscriptions (
    subscription_id     uuid        PRIMARY KEY,
    organization_id     uuid        NOT NULL REFERENCES oxsum.organizations (organization_id) ON DELETE RESTRICT,
    plan_id             uuid        NOT NULL REFERENCES oxsum.subscription_plans (plan_id) ON DELETE RESTRICT,
    state               text        NOT NULL CHECK (state IN ('active', 'cancelled', 'expired')),
    -- The window `consumed_minor` counts inside. `current_period_start` is when
    -- this window opened (subscription bind or the last rollover);
    -- `current_period_end` is when it closes, NULL for a `never`-resetting
    -- plan: with no boundary there is nothing to roll and nothing to expire —
    -- only a cancel or the rail's `deleted` event ends it.
    current_period_start timestamptz NOT NULL,
    current_period_end   timestamptz,
    consumed_minor      bigint      NOT NULL DEFAULT 0 CHECK (consumed_minor >= 0),
    -- Snapshots of the plan's terms at bind time, so a mid-life plan edit does
    -- not move a subscription that was sold under different terms: the quota
    -- `consumed_minor` runs against, and whether the remainder may overflow.
    quota_minor         bigint      CHECK (quota_minor IS NULL OR quota_minor > 0),
    allow_wallet_overflow boolean   NOT NULL DEFAULT true,
    -- What opened the link: 'admin' for an operator bind, 'stripe' for a
    -- paid rail. Free-form enough to name a rail without a second table.
    source              text        NOT NULL DEFAULT 'admin',
    -- The Stripe subscription id the webhook renews against, when `source` is
    -- stripe. Unique so two subscriptions never claim one Stripe object.
    stripe_subscription_id text     UNIQUE,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);

-- At most one active subscription per organization: the unique partial index
-- is the constraint the upgrade/downgrade transaction serializes on.
CREATE UNIQUE INDEX org_subscriptions_one_active
    ON oxsum.org_subscriptions (organization_id)
    WHERE state = 'active';

-- The rollover pass's scan: rows whose period has ended.
CREATE INDEX org_subscriptions_period_end
    ON oxsum.org_subscriptions (current_period_end)
    WHERE state = 'active' AND current_period_end IS NOT NULL;

-- Every period a subscription has run, newest readable by start. The rollover
-- writes one row per boundary; the row a period closes on stays as the record
-- of what it consumed against what quota.
CREATE TABLE oxsum.subscription_periods (
    period_id           uuid        PRIMARY KEY,
    subscription_id     uuid        NOT NULL REFERENCES oxsum.org_subscriptions (subscription_id) ON DELETE RESTRICT,
    organization_id     uuid        NOT NULL REFERENCES oxsum.organizations (organization_id) ON DELETE RESTRICT,
    plan_id             uuid        NOT NULL REFERENCES oxsum.subscription_plans (plan_id) ON DELETE RESTRICT,
    period_start        timestamptz NOT NULL,
    -- NULL while the period is open — `never`-reset plans and the
    -- in-flight window a rollover has not closed yet.
    period_end          timestamptz,
    -- The quota the plan carried into the period: a snapshot, so a plan edit
    -- mid-period does not retroactively resize the window it opened.
    quota_minor         bigint      CHECK (quota_minor IS NULL OR quota_minor > 0),
    consumed_minor      bigint      NOT NULL DEFAULT 0 CHECK (consumed_minor >= 0),
    -- How the period closed: 'boundary' for a rollover, 'cancelled' for an
    -- operator or rail cancel, 'expired' for a lapsed subscription,
    -- 'upgrade'/'downgrade' for a plan change. 'open' while it runs.
    outcome             text        NOT NULL CHECK (outcome IN ('open', 'boundary', 'cancelled', 'expired', 'upgrade', 'downgrade')),
    created_at          timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX subscription_periods_subscription
    ON oxsum.subscription_periods (subscription_id, period_start);

-- One row per hold-time draw: the subscription's share of a freeze. `hold_key`
-- is the same `req-<id>:hold` idempotency key the ledger hold takes, so the
-- draw and the hold live and die under the one name. `settled_minor` is what
-- the settlement charged against the draw — the release is `drawn - settled`.
CREATE TABLE oxsum.subscription_draws (
    draw_id             uuid        PRIMARY KEY,
    subscription_id     uuid        NOT NULL REFERENCES oxsum.org_subscriptions (subscription_id) ON DELETE RESTRICT,
    period_id           uuid        NOT NULL REFERENCES oxsum.subscription_periods (period_id) ON DELETE RESTRICT,
    hold_key            text        NOT NULL UNIQUE,
    drawn_minor         bigint      NOT NULL CHECK (drawn_minor >= 0),
    settled_minor       bigint      CHECK (settled_minor IS NULL OR settled_minor >= 0),
    created_at          timestamptz NOT NULL DEFAULT now(),
    settled_at          timestamptz
);

CREATE INDEX subscription_draws_period ON oxsum.subscription_draws (period_id);
