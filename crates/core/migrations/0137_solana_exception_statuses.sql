-- The Solana rail's operator exception queue and pull-payment refund flow
-- (issue #1142): a deposit whose paying transfer is verified but cannot
-- settle — underpaid, overpaid, landed after the intent expired, or held for
-- sanctions review — must stop masquerading as `pending`, and a refund is a
-- business decision with a state of its own, not a silent refusal.
--
-- `underpaid`: a located transfer honored the intent but delivered less than
--   the asked amount — the asked figure is per-asset base units under the
--   multi-asset whitelist (issue #1140): lamports for a `sol` intent, the
--   locked mint's base units for an SPL one. The opened amount is not
--   credited; the shortfall sits in `meta.delivered`/`received_minor` for a
--   human.
-- `overpaid`: a located transfer delivered more than asked. The opened
--   amount credits normally (`entry_id` is set — the money-in record stands)
--   and the status itself marks the surplus for the queue.
-- `late`: the paying transaction's on-chain `blockTime` is at/after the
--   intent's `expiresAt`. Formerly an indistinguishable refusal that left the
--   row `pending` forever; now a terminal queue marker.
-- `flagged`: reserved for pre-credit sanctions screening (issue #1144) — the
--   status and its queue surface exist now; the screening sets it later.
-- `refund_pending`: an operator opened a pull-payment refund (BTCPay-style):
--   the customer supplies the refund address, the operator approves, and the
--   multisig pays out-of-band — the server holds no spend key, so nothing
--   auto-sends.
-- `refunded`: the payout's on-chain signature was recorded back and the
--   deposit is closed. For a deposit that credited (`overpaid`) the surplus
--   was returned; for one that never credited the delivered funds were
--   returned — in the asset the intent locked (`meta.asset`), never the
--   credit figure. Terminal.
--
-- The generic statuses stay untouched: `pending`, `confirmed`, `credited`,
-- `reversed`, `expired`. Transitions are monotonic — `underpaid`/`late`/
-- `overpaid` are terminal markers the verify path writes instead of
-- refusing, `refund_pending`/`refunded` the refund lifecycle, and the
-- `entry_id IS NOT NULL` fact — not the status name — is what proves a
-- deposit net-credited the purchased pool (see the 2026-11-14 entry in
-- docs/decisions.md).

-- Numbering/hand-off: this migration lands after 0136's `flagged`-only
-- CHECK (issue #1144); rewriting the constraint here keeps `flagged` and
-- adds the rest of the exception set, so the CHECK below is the complete
-- vocabulary, not a delta — the standing rule for two migrations that touch
-- one CHECK.

ALTER TABLE oxsum.deposits
    DROP CONSTRAINT deposits_status_check,
    ADD CONSTRAINT deposits_status_check
        CHECK (status IN (
            'pending', 'confirmed', 'credited', 'reversed', 'expired',
            'underpaid', 'overpaid', 'late', 'flagged',
            'refund_pending', 'refunded'
        ));
