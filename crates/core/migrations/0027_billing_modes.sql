-- Embeddings and rerank metering (issue #170, roadmap P8-3): `channel_prices.mode`
-- was reserved at the itemized-price-book migration; this change makes the
-- input-only modes storable.
--
-- An embeddings or rerank price has no output side, so `max_output_tokens` is
-- zero on those rows rather than positive: the check relaxes from `> 0` to
-- `>= 0`, and the mode-aware validation in core keeps chat rows honest
-- (a chat price with no output ceiling is still refused at write time).

ALTER TABLE oxsum.channel_prices
    DROP CONSTRAINT channel_prices_max_output_tokens_check,
    ADD CONSTRAINT channel_prices_max_output_tokens_check CHECK (max_output_tokens >= 0);
