-- Tag-scoped token rate limits (issue #209, follow-up to #203). The protection
-- decision (docs/decisions.md, 2026-10-05, "Attribution") scopes a key's TPM
-- allowance to a request tag in three forms — Each (a bucket per tag value),
-- Concrete (one named value), Total (the whole tag) — on top of the plain
-- per-key window #203 shipped.
--
-- A scope is just a name for which slice of the key's settled tokens a row
-- counts, so it lands as a column on `oxsum.tpm_windows` rather than a sibling
-- table: the plain window keeps `scope = ''`, and scoped rows ride the same
-- trailing-minute sum and the same prune-on-write. Scope names are built by
-- `Db`'s accrual (`tag:<name>=<value>` for Each/Concrete, `tag:<name>` for
-- Total) and can never collide with the empty scope, because a real scope
-- always carries a tag name.
--
-- Cardinality stays bounded by construction: a per-value (Each) row is still a
-- trailing-minute bucket, so it expires with the window — a high-cardinality
-- tag value set cannot accumulate rows beyond 60 seconds.

-- The window's identity gains its scope; existing rows default to the plain
-- bucket, so the reshape is additive over anything already accrued.
ALTER TABLE oxsum.tpm_windows
    ADD COLUMN scope text NOT NULL DEFAULT '';
ALTER TABLE oxsum.tpm_windows
    DROP CONSTRAINT tpm_windows_pkey;
ALTER TABLE oxsum.tpm_windows
    ADD PRIMARY KEY (key_id, scope, window_start);

-- The scopes a key carries, kept as JSON so the three scope forms stay one
-- column: {"team": {"each": 5000}, "tier": {"concrete": "free",
-- "tokensPerMinute": 1000}, "region": {"total": 20000}}. NULL is no tag scopes,
-- the same "uncapped" reading `tokens_per_minute` itself takes.
ALTER TABLE oxsum.api_keys
    ADD COLUMN tokens_per_minute_scopes jsonb;
