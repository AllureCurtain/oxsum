-- External witness anchoring (issue #231): the checkpoint an outside witness
-- last cosigned per organization, and every cosigned note it ever answered.
--
-- `witness_checkpoints` is one row per organization — the last head the
-- `witness-checkpoints` pass published, in the C2SP signed-note text the
-- signature actually covers — and `witness_cosignatures` is the append-only
-- trail of the notes each witness returned: keyed by
-- (organization, witness_url, head_size) so a re-delivered checkpoint
-- idempotently records once, and so a later head's cosignatures never rewrite
-- an earlier head's trail.
CREATE TABLE oxsum.witness_checkpoints (
    organization_id uuid        NOT NULL REFERENCES oxsum.organizations (organization_id) ON DELETE CASCADE,
    head_size       bigint      NOT NULL CHECK (head_size >= 0),
    note            text        NOT NULL,
    updated_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (organization_id)
);

COMMENT ON COLUMN oxsum.witness_checkpoints.note IS
    'the operator-signed C2SP note covering the head at head_size, byte for byte';

CREATE TABLE oxsum.witness_cosignatures (
    organization_id uuid        NOT NULL REFERENCES oxsum.organizations (organization_id) ON DELETE CASCADE,
    witness_url     text        NOT NULL,
    head_size       bigint      NOT NULL CHECK (head_size >= 0),
    note            text        NOT NULL,
    first_seen      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (organization_id, witness_url, head_size)
);

COMMENT ON COLUMN oxsum.witness_cosignatures.note IS
    'the cosigned C2SP note the witness returned for this head — its signature line is the verifiable part';
