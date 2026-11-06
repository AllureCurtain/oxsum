-- Customer evidence bundles and their share tokens (issue #616, task T25).
--
-- `oxsum.evidence_bundles` is the minted descriptor: one row per
-- `(organization, kind, period_from, period_to, statement_id)`, the same
-- once-per-scope rule `usage_reports` keeps — a re-request replays the
-- standing bundle rather than committing a second manifest. The bundle's
-- ledger marker entry (`oxsum/evidence-bundle/v1`) binds the manifest digest
-- the artifacts are served against, and `bundle_id` is that marker entry's
-- id, so `GET /api/v1/entries/{id}/proof` resolves it like every other
-- committed marker.
--
-- The artifacts themselves are not stored: a download re-reads the same
-- tables the manifest digests commit, and the verifier re-digests them — a
-- table row that drifted after mint fails the manifest's digest rather than
-- being silently re-served (the same posture `usage_reports` takes by
-- freezing its row list at mint; here the manifest digest *is* the freeze).
--
-- `oxsum.evidence_share_tokens` is the capability a share URL carries — a
-- hashed token like `report_links`, plus the download budget a bill link
-- does not need: `max_downloads` bounds how many times a link resolves, and
-- `download_count`/`last_download_at` are the audit figures. A resolved
-- download increments the counter in the same UPDATE that checks the bounds,
-- so two raced downloads cannot both pass a remaining budget of one.

CREATE TABLE oxsum.evidence_bundles (
    -- The marker entry's id — `GET /api/v1/entries/{id}/proof` resolves it.
    bundle_id         uuid PRIMARY KEY,
    organization_id   uuid NOT NULL REFERENCES oxsum.organizations(organization_id) ON DELETE RESTRICT,
    -- `period` covers a closed UTC day span; `statement` one plan statement.
    kind              text NOT NULL CHECK (kind IN ('period', 'statement')),
    -- The covered span for `period`; NULL pair for `statement`.
    period_from       date,
    period_to         date,
    -- The `plan_statements` row a `statement` bundle wraps; NULL for `period`.
    statement_id      uuid,
    -- The manifest digest the marker's DocumentRef binds, lowercase hex.
    manifest_digest   text NOT NULL CHECK (manifest_digest ~ '^[0-9a-f]{64}$'),
    -- The marker's position in the organization's ledger.
    anchor_log_index  bigint NOT NULL,
    -- The bundle's own JSON at mint: the artifacts re-serialize to the same
    -- digests while the underlying rows stand, so a stored copy keeps a mint
    -- replay byte-stable against later catalog drift — the same reason the
    -- usage report's rows are frozen at mint. Bytea rather than jsonb so the
    -- bytes the digest commits survive verbatim.
    bundle_json       bytea NOT NULL,
    entry_count       bigint NOT NULL,
    created_at        timestamptz NOT NULL DEFAULT now(),
    CHECK ((kind = 'period') = (period_from IS NOT NULL AND period_to IS NOT NULL)),
    CHECK ((kind = 'statement') = (statement_id IS NOT NULL))
);
CREATE UNIQUE INDEX evidence_bundles_period
    ON oxsum.evidence_bundles (organization_id, period_from, period_to)
    WHERE kind = 'period';
CREATE UNIQUE INDEX evidence_bundles_statement
    ON oxsum.evidence_bundles (organization_id, statement_id)
    WHERE kind = 'statement';
CREATE INDEX evidence_bundles_org ON oxsum.evidence_bundles (organization_id, created_at DESC);

CREATE TABLE oxsum.evidence_share_tokens (
    share_id          uuid PRIMARY KEY,
    bundle_id         uuid NOT NULL REFERENCES oxsum.evidence_bundles(bundle_id) ON DELETE CASCADE,
    organization_id   uuid NOT NULL REFERENCES oxsum.organizations(organization_id) ON DELETE RESTRICT,
    -- SHA-256 of the presented token — the plaintext is answered once at mint.
    token_hash        bytea NOT NULL UNIQUE,
    label             text,
    expires_at        timestamptz NOT NULL,
    -- The download budget: NULL is unbounded downloads until expiry, a figure
    -- counts remaining serves. `download_count` only ever rises, and the
    -- resolve checks `< max_downloads` before incrementing.
    max_downloads     integer CHECK (max_downloads IS NULL OR max_downloads > 0),
    download_count    integer NOT NULL DEFAULT 0 CHECK (download_count >= 0),
    revoked_at        timestamptz,
    last_download_at  timestamptz,
    created_by        uuid REFERENCES oxsum.users(user_id),
    created_at        timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX evidence_share_tokens_bundle
    ON oxsum.evidence_share_tokens (bundle_id, created_at DESC);
