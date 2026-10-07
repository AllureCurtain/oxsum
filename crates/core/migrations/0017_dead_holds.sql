-- The sweeper's bookkeeping on a watched hold. A settle that keeps failing
-- bumps sweep_attempts and stores the last error; past ten attempts the row
-- is dead-lettered — dead_at marks it and gates it to an hourly retry rather
-- than every pass.
ALTER TABLE oxsum.open_holds
    ADD COLUMN sweep_attempts integer NOT NULL DEFAULT 0,
    ADD COLUMN last_error text,
    -- When the sweeper last tried the row: the hourly retry's cadence gate
    -- once the row is dead.
    ADD COLUMN last_attempt_at timestamptz,
    -- Set once, when sweep_attempts reaches the dead-letter threshold. A dead
    -- row needs an operator; the sweeper keeps retrying it hourly.
    ADD COLUMN dead_at timestamptz;
