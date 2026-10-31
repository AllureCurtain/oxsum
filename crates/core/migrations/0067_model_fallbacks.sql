-- Model-level fallbacks (issue #500): the ordered chain of models a request
-- may serve under when its own model's every channel fails — 'gpt-4o
-- unavailable → try gpt-4o-mini'. The client asks for a capability tier; the
-- gateway picks which model actually answers.
--
-- The chain is model → model, channel-agnostic: a model may be priced on
-- several channels, and a fallback resolves through the same `route_order`
-- each model's own servings produce — it is a *named* model, never a channel.
-- That is why it is its own table and not a column: `oxsum.channel_prices` is
-- append-only (a version's figures never change after write, so a bill always
-- proves the price it was charged under), and a chain that edits with config
-- cannot ride a row a trigger forbids changing. `model_fallbacks` is mutable
-- config, not ledger.
CREATE TABLE IF NOT EXISTS oxsum.model_fallbacks (
    -- The model the chain hangs off — the name a caller asks for.
    model          text        NOT NULL,
    -- The fallback's place in the chain, 0-based: position 0 is tried first.
    position       integer     NOT NULL CHECK (position >= 0),
    -- The model the request may serve under at this position — resolved by
    -- `servings_named` like any requested model, so its own channels and
    -- aliases apply.
    fallback_model text        NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (model, position)
);
