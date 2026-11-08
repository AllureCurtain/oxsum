# Pool exhaustion deadline regression

Issue #862 repairs the failure observed in main CI 42518492586 at `42b80a24`:
`pool_exhaustion_fails_fast_and_recovers` let SQLx and its outer watchdog both
expire at 30 seconds. Whichever timer was polled first decided the result, and
the old match also permitted a successful balance query despite full saturation.

Only this fault fixture now supplies a one-second acquire deadline. A separate
five-second watchdog detects a genuine hang. The test holds all 16 configured
connections, requires `WalletError::PoolBusy`, releases them and verifies that
exactly 5,000,000 minor units remain available. Other fault fixtures and production
pool configuration are unchanged.

On source `31d8dffa` with shared-read runtime `43a08320`:

- All 17 PostgreSQL fault checks passed, zero skipped, in 5.008 seconds. The
  saturation case completed in 2.616 seconds including setup/recovery.
- Strict workspace/all-target Clippy passed in 1m56s; formatter/diff checks passed.
- Independent source review passed. The shared-read parent separately passed
  2,012 full workspace checks; no additional full run is needed for this fixture.

The prior shared-read full run also passed the old saturation test in 33.528s;
that did not eliminate the demonstrated deadline race. This repair is validated
by the explicit PoolBusy deadline and recovery assertions, not repeated luck.
