-- Failover: one model may be served by several channels (issue #168, roadmap
-- P8-2). `channel_prices.weight` is the route's relative preference for leading
-- a request — versioned with the price, so the settlement names the routing in
-- force by the same version that priced it. `usage_records.upstream_attempts`
-- counts how many upstream calls a turn made: above 1 means it failed over
-- between channels under the same hold, or waited out a bounded `Retry-After`
-- on a lone channel's 429.

ALTER TABLE oxsum.channel_prices
    ADD COLUMN weight integer NOT NULL DEFAULT 100
        CHECK (weight BETWEEN 1 AND 1000);

ALTER TABLE oxsum.usage_records
    ADD COLUMN upstream_attempts integer NOT NULL DEFAULT 1
        CHECK (upstream_attempts >= 1);
