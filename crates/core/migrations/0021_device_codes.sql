-- Issue #156 (roadmap P6-4): device authorization. One row per request a tool
-- mints through `POST /api/v1/device/code`: the device code polls, the user
-- code is typed at the approval page, and neither is stored in the clear —
-- both columns hold SHA-256 like every token the project mints.
CREATE TABLE oxsum.device_codes (
    -- SHA-256 of the `oxd-` device code, the only form the database holds.
    -- Also the poll's lookup: the unique index on this column is what
    -- authenticates a token request.
    device_code_hash bytea        PRIMARY KEY,
    -- SHA-256 of the normalized user code (`XXXX-XXXX`, uppercase, dashes
    -- stripped). Unique, so two live requests can never share what a user
    -- would type.
    user_code_hash   bytea        NOT NULL UNIQUE,
    -- The user code as the tool displayed it, `XXXX-XXXX`. Not a credential —
    -- the device code is — so it is kept in the clear for the approval page's
    -- echo and the minted key's name.
    user_code        text         NOT NULL,
    -- pending until a session stamps it; delivered once the minted key's
    -- secret has been answered to a poll. Terminal states are denied and
    -- delivered.
    status           text         NOT NULL DEFAULT 'pending'
                                  CHECK (status IN ('pending', 'approved', 'denied', 'delivered')),
    created_at       timestamptz  NOT NULL DEFAULT now(),
    expires_at       timestamptz  NOT NULL,
    -- Rate-limit input for the poll leg: a poll inside `interval` seconds is
    -- RATE_LIMITED rather than a second look at the row.
    last_poll_at     timestamptz,
    -- Who approved, and which organization the minted key spends for. Both
    -- are written only by the approve verdict, inside one statement, so a
    -- request can never be approved for an organization the approver was not
    -- acting in. SET NULL on the user mirrors api_keys.created_by — a request
    -- outlives the account that answered it.
    approved_by      uuid         REFERENCES oxsum.users (user_id) ON DELETE SET NULL,
    organization_id  uuid         REFERENCES oxsum.organizations (organization_id) ON DELETE RESTRICT
);
