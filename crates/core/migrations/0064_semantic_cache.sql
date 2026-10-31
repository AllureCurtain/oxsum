-- The semantic response cache (issue #497): a cached upstream answer an
-- organization may be re-served for a near-duplicate request, settled at the
-- cache-hit price rather than a fresh turn's.
--
-- One row per (organization, channel, model, request-shape digest): the digest
-- is `oxsum/prompt/v1` over the surface's normalized prompt plus the model, so
-- an identical ask under the same channel+model is an exact hit. The answer
-- keeps the response body and the usage the original turn reported, so a hit
-- can settle by the same itemization path a fresh turn uses — `cacheHit: true`
-- on the settle tells it apart, and `source_entry_id` names the original
-- ledger entry the answer came from, which is the proof's link.
--
-- Per-organization scoping is the privacy boundary: a cached entry is never
-- shared across orgs, and within one org the org's keys share it by default —
-- the equivalent of bifrost's `disable_identity_scoping`, flipped: inside the
-- customer the cache is a shared asset, outside it is a leak.

CREATE TABLE oxsum.cached_responses (
    cache_id         uuid        PRIMARY KEY,
    -- The organization whose turn produced the answer; the lookup's first
    -- filter, and the row's owner for every later policy question.
    organization_id  uuid        NOT NULL REFERENCES oxsum.organizations (organization_id) ON DELETE CASCADE,
    -- The channel that answered. A hit settles under this channel's current
    -- price version for the model — the bill says which route's rate the
    -- reuse was billed at, the same as a fresh turn.
    channel_id       uuid        NOT NULL REFERENCES oxsum.channels (channel_id) ON DELETE RESTRICT,
    -- The public model name the request asked for.
    model            text        NOT NULL,
    -- BLAKE3 (`oxsum/prompt/v1`) over the surface's normalized prompt plus the
    -- model: the key a duplicate request reproduces exactly. Prompt text is
    -- normalized before hashing — lowercased and whitespace-folded — so the
    -- cache's "same question" is a stable one.
    prompt_digest    text        NOT NULL,
    -- The upstream's answer, verbatim: what a hit serves. Only a non-streamed
    -- answer is ever filed — a streamed turn skips both lookup and store, so a
    -- hit is always re-served as a normal non-streamed response, because the
    -- client asked a question, not a delivery format.
    response_body    jsonb       NOT NULL,
    -- The turn's usage the original call reported, so the hit's settle
    -- itemizes what upstream actually counted rather than a zero-count
    -- record — the cache-hit charge prices *the reused answer*, which is the
    -- honest figure.
    usage_json       jsonb       NOT NULL,
    -- The ledger entry the answer was produced under, which is the link a
    -- `cacheHit` bill proves reuse against.
    source_entry_id  uuid        NOT NULL,
    -- How many times the row has served a request. The admin surface reads it;
    -- freshness is `created_at + ttl_seconds`, never hit count.
    hit_count        bigint      NOT NULL DEFAULT 0,
    -- Per-row TTL: `created_at + ttl_seconds` past is a stale row the lookup
    -- never answers. `NULL` means it does not expire — a deployment that sells
    -- a forever-reusable answer declares so.
    ttl_seconds      bigint,
    created_at       timestamptz NOT NULL DEFAULT now(),
    -- The exact-hit index: same org, same channel, same model, same digest.
    UNIQUE (organization_id, channel_id, model, prompt_digest)
);

-- The live-lookup index mirrors the UNIQUE key: a stale row still indexes and
-- is deleted at read time — an expiry predicate here would need `now()`, which
-- is not IMMUTABLE and so cannot stand in an index's WHERE.
CREATE INDEX cached_responses_org_lookup ON oxsum.cached_responses
    (organization_id, channel_id, model, prompt_digest);

-- A channel's cache policy: `settings.semanticCache` reads
-- `{enabled, ttlSeconds, excludeSystem}` — absent means disabled, because a
-- cache that was never asked for must never answer. `Serving` carries the
-- parsed policy so the gateway checks it at route time, not by re-reading the
-- row.
ALTER TABLE oxsum.channels
    ADD COLUMN settings jsonb NOT NULL DEFAULT '{}';
