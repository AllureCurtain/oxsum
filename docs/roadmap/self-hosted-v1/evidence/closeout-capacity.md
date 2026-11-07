# Closeout: measured streaming capacity and conservation

Issue [#685](https://github.com/AllureCurtain/oxsum/issues/685), A28/D09.
Measured on 2026-11-07 against base `3f81a6c35f097feb0f4dc6bb15cc348c8ed5f371`
plus this PR's harness. macOS arm64, 12 logical CPUs / 24 GiB RAM; PostgreSQL
17.11 in Docker Desktop configured with 12 CPUs / 8 GiB, 30 pool connections.
This is development hardware, not a 4-vCPU certification.

## Reproduce

```bash
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres \
SOAK_REPORT=/tmp/capacity-d09.json scripts/soak.sh --d09
```

The ignored test uses a real HTTP listener, production router, local streaming
provider and an isolated scratch database that is dropped on process exit.
One fixture identity owns 1,000 independent billing accounts; provisioning via
production domain APIs creates 1,000 ledger schemas and 10,000 actual keys.
Round-robin traffic visits all accounts, rotating keys per account. Each stream
emits a first token, waits 5 seconds, then reports 6 input + 1 output token.
The exact charge is 7 minor units. Concurrency is bounded at 100; admission is
scheduled at 20/s with missed ticks skipped instead of bursting.

## Result

Raw [JSON](artifacts/capacity-d09-2026-11-07.json) and
[log](artifacts/capacity-d09-2026-11-07.log) are retained. The test passed.

| Measurement | Observed |
| --- | ---: |
| Admission interval / total including drain | 300.001 s / 305.031 s |
| Successful / failed requests | 5,897 / 0 |
| Admission rate / completed rate including drain | 19.657 / 19.332 per second |
| Peak upstream streams | 100 |
| TTFT p50 / p95 / p99 | 24.051 / 81.443 / 174.991 ms |
| Total p50 / p95 / p99 | 5,042.961 / 5,113.421 / 5,214.053 ms |
| p95 total minus scripted 5-second delay | 113.421 ms |
| Operations / successful correctly priced operations / usage rows | 5,897 / 5,897 / 5,897 |
| Expected / operation / usage charge | 41,279 / 41,279 / 41,279 minor |
| Sum of absolute per-wallet balance drift | 0 minor |
| Remaining open holds / reserved balance | 0 / 0 minor |

Nearest-rank percentiles are unit tested. Settlement gets up to 30 seconds to
finish; no sweeper is run to conceal a leaked reservation. The conservation
assertions compare each wallet with its opening balance minus its own calls,
not only a global sum that could hide a cross-customer offset.

## Limits and follow-up

D09's >=20/s and <100 ms p95 overhead target is **not met** by this run.
The configured limit plus five-second streams leaves little spare capacity;
results must stay a declared release limitation, not silently redefine the
threshold. The overhead figure includes loopback network, scheduling and final
settlement, so it is an upper bound rather than isolated server processing.

The load process runs the production router without the background job runner;
job crash/retry storms and busy/quiet tenant fairness are separately measured
in [T27](T27.md). This is a five-minute controlled run, not a long-duration leak
test or PSP/real-model certification. A frontend build briefly ran during the
measurement; no other Cargo test or database workload was intentionally run.
The raw `provisioning_seconds` field was an approximation including final
reconciliation; the harness now snapshots wallet/key provisioning separately
and emits admission throughput explicitly. Neither reporting correction alters
these request latencies or the accounting assertions.
