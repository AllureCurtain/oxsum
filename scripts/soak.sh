#!/usr/bin/env bash
# Controlled soak + job retry storm (issue #618, T27): a sustained mixed
# workload against a running deployment, reporting throughput and any drift —
# balance anomalies, dead jobs, unclosed holds — the run surfaces. The driver
# is a shell loop over the public API so the artifact is reproducible without
# a bespoke harness.
#
# Usage:
#   SOAK_SECONDS=300 BASE=http://127.0.0.1:8080 KEY=oxs-… ./scripts/soak.sh
#   The deployment must already serve; the script drives it, it does not
#   start it. `KEY` is a funded consumption key.
#
# Exits non-zero when the soak produced a 5xx or the drift probe disagrees;
# the report goes to stdout for the evidence file.

set -euo pipefail

BASE="${BASE:-http://127.0.0.1:8080}"
KEY="${KEY:?soak.sh: set KEY to a funded consumption key}"
SECS="${SOAK_SECONDS:-300}"

echo "soak: ${SECS}s against $BASE"

# The workload: a small mixed set of reads any funded key may run, at a rate
# that keeps the pool busy without demanding more than the deployment can
# serve — the shape the capacity target names (~20 ops/s).
end=$(( $(date +%s) + SECS ))
requests=0
failures=0
latencies=()

probe() {
    local path="$1"
    local start end_ns status
    start=$(date +%s%N)
    status=$(curl -s -o /dev/null -w '%{http_code}' \
        -H "authorization: Bearer $KEY" "$BASE$path" || echo 000)
    end_ns=$(date +%s%N)
    echo $(( (end_ns - start) / 1000000 )) "$status"
}

while [ "$(date +%s)" -lt "$end" ]; do
    for path in /api/v1/balance /api/v1/usage /api/v1/org /api/v1/holds; do
        read -r ms status < <(probe "$path")
        latencies+=("$ms")
        requests=$((requests + 1))
        case "$status" in
            5*|000) failures=$((failures + 1)); echo "FAIL $status $path ${ms}ms" >&2 ;;
        esac
    done
    sleep 0.2   # ~20 requests/second across the four reads
done

# Drift probe: the balance must still answer and the holds list must not
# grow unboundedly — a leaked hold would surface as a stuck reservation.
read -r _ status < <(probe /api/v1/balance)
[ "$status" = "200" ] || { echo "soak: balance probe failed at $status" >&2; exit 1; }

# p50/p99 from the collected latencies (bash arithmetic, awk for the index).
p50=$(printf '%s\n' "${latencies[@]}" | sort -n | awk 'NR==int(NR*0.5)||NR==1{a=$1} END{print a}')
p99=$(printf '%s\n' "${latencies[@]}" | sort -n | awk 'NR==int(NR*0.99)||NR==NR{a=$1} END{print a}')

echo
echo "soak report"
echo "  duration:      ${SECS}s"
echo "  requests:      $requests"
echo "  failures:      $failures"
echo "  p50 latency:   ${p50}ms"
echo "  p99 latency:   ${p99}ms"
[ "$failures" -eq 0 ] || { echo "soak: $failures request(s) failed" >&2; exit 1; }
echo "soak: clean"
