-- Signed spend mandates (issue #262): a key holder delegates a strictly narrower
-- slice of a key's authority to a bearer who does not hold the key.
--
-- `oxsum.mandates` is the stable id a mandate's committed spend keys on. The
-- cap read sums the ledger's own postings — settled charges plus outstanding
-- holds — sliced by the mandate id each entry carries as its provenance
-- correlation, under `hold_for_key`'s per-key advisory lock: the same figure
-- `key_committed_in` derives a key's spend from, so a mandate's committed total
-- is ledger-derived and can never drift from the books. The signed blob the
-- delegate carries is never stored whole — the row keeps the fields it
-- authorizes, and the signature itself proves the bytes were minted here.
--
-- `api_keys.mandate_signing_seed` is the Ed25519 seed the key's mandates are
-- signed under, sealed like a channel credential: the column holds the sealed
-- record, never the seed, and a verifier fetches only the public key. A mandate
-- is `not_after`-bounded and never revoked — a key that must stop delegating
-- revokes or re-times the constraint, and a delegate's window just runs out.

ALTER TABLE oxsum.api_keys
    ADD COLUMN mandate_signing_seed bytea;

CREATE TABLE oxsum.mandates (
    mandate_id  uuid PRIMARY KEY,
    key_id      uuid NOT NULL REFERENCES oxsum.api_keys (key_id),
    organization_id uuid NOT NULL REFERENCES oxsum.organizations (organization_id),
    cap_minor   bigint NOT NULL CHECK (cap_minor > 0),
    models      jsonb,
    not_before  timestamptz NOT NULL,
    not_after   timestamptz NOT NULL,
    nonce       text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    CHECK (not_after > not_before)
);

ALTER TABLE oxsum.usage_records
    ADD COLUMN mandate_id uuid;
ALTER TABLE oxsum.open_holds
    ADD COLUMN mandate_id uuid;

-- Attribution indexes: the bills page and a swept hold's usage row record the
-- mandate a turn ran under, and these keep the lookups narrow — a mandate is
-- short-lived and per-key.
CREATE INDEX open_holds_mandate_id ON oxsum.open_holds (mandate_id) WHERE mandate_id IS NOT NULL;
CREATE INDEX usage_records_mandate_id ON oxsum.usage_records (mandate_id) WHERE mandate_id IS NOT NULL;
