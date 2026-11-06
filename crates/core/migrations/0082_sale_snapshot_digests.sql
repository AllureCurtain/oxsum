-- Resolved customer sale snapshots, keyed by the digest a settlement commits.
--
-- A sale-priced settlement's description commits `sale.digest` — the resolved
-- SalePriceSnapshot's tagged-BLAKE3 hash — and the proof bundle's `sale` link
-- must attest the canonical snapshot bytes that produced it. Signing is
-- deterministic over the snapshot, so the row the digest names is stored once
-- and re-served verbatim forever: a bill stays verifiable after the sale book
-- moves on, and the bundle carries the canonical row the verifier re-digests.
-- The digest is content-derived, so the insert is idempotent — two resolutions
-- of the same terms write the same row and ON CONFLICT does nothing.
CREATE TABLE oxsum.sale_snapshot_digests (
    digest text PRIMARY KEY CHECK (digest ~ '^[0-9a-f]{64}$'),
    business_id uuid NOT NULL REFERENCES oxsum.businesses,
    organization_id uuid NOT NULL,
    model text NOT NULL,
    -- The canonical SaleSnapshotV1 bytes the digest was taken over — the exact
    -- string the checkpoint's root commits to, never re-rendered.
    canonical_row text NOT NULL,
    -- The snapshot's book version, carried in the checkpoint's size slot
    -- (0 when the snapshot names none).
    version bigint NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY (business_id, organization_id) REFERENCES oxsum.customer_profiles (business_id, organization_id)
);
-- The sale-head endpoint re-resolves by (organization, model, instant); the
-- digest lookup above is the proof path, this index serves the by-terms read.
CREATE INDEX sale_snapshot_digests_terms
    ON oxsum.sale_snapshot_digests (business_id, organization_id, model);
