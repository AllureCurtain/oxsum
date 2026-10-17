-- oxsum as a tlog-witness (issue #260): the state this deployment's witness
-- keeps for the foreign logs it tracks, and the append-only trail of every
-- cosigned note it issued.
--
-- `witness_served` is one row per tracked origin — the last head the witness
-- accepted. It is the mutable half of the WitnessStore contract: the trust
-- root `OXSUM_WITNESS_TRUST` seeds a row, and each accepted checkpoint moves
-- it forward. `witness_vouched` is the append-only half — every cosigned note
-- the witness returned, keyed by (origin, head_size) so a re-offer records
-- once and a later head's voucher never rewrites an earlier one's.

CREATE TABLE oxsum.witness_served (
    origin      text        NOT NULL,
    head_size   bigint      NOT NULL CHECK (head_size >= 0),
    root_b64    text        NOT NULL,
    updated_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (origin)
);

COMMENT ON TABLE oxsum.witness_served IS
    'the last head this deployment''s tlog-witness accepted per tracked origin (issue #260)';
COMMENT ON COLUMN oxsum.witness_served.root_b64 IS
    'the head root, base64, as the C2SP note body carries it';

CREATE TABLE oxsum.witness_vouched (
    origin      text        NOT NULL,
    head_size   bigint      NOT NULL CHECK (head_size >= 0),
    note        text        NOT NULL,
    issued_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (origin, head_size)
);

COMMENT ON TABLE oxsum.witness_vouched IS
    'the cosigned notes this deployment''s tlog-witness returned, append-only (issue #260)';
COMMENT ON COLUMN oxsum.witness_vouched.note IS
    'the checkpoint note plus this witness''s cosignature/v1 signature line, byte for byte';
