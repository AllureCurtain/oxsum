-- Response cache v2 (issue #645): the digest now binds the complete admitted
-- request and serving context. Pre-upgrade rows were keyed by a normalized
-- prompt only and can never hit the new key; a cached answer is a rebuildable
-- projection, so stale rows are discarded rather than migrated across an
-- incomplete context (D04).
DELETE FROM oxsum.cached_responses;
