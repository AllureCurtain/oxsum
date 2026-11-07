#!/usr/bin/env bash
# Isolated streaming gateway load and accounting conservation (#685).
# Default: small development workload. --d09: 1,000 billing accounts,
# 10,000 keys, 100 concurrent streams and 20 admissions/s for five minutes.
# Uses a local scripted provider and a per-run scratch database, dropped on exit.
# No real provider, payment credentials or existing deployment is used.
set -euo pipefail
cd "$(dirname "$0")/.."
if [ "${1:-}" = "--d09" ]; then
    export SOAK_CUSTOMERS="${SOAK_CUSTOMERS:-1000}"
    export SOAK_KEYS_PER_CUSTOMER="${SOAK_KEYS_PER_CUSTOMER:-10}"
    export SOAK_CONCURRENCY="${SOAK_CONCURRENCY:-100}"
    export SOAK_RATE="${SOAK_RATE:-20}"
    export SOAK_SECONDS="${SOAK_SECONDS:-300}"
    export SOAK_STREAM_MS="${SOAK_STREAM_MS:-5000}"
elif [ "$#" -gt 0 ]; then
    echo "usage: scripts/soak.sh [--d09]" >&2
    exit 2
fi
# nextest streams the report with --no-capture; a missing DB fails the test.
cargo nextest run -p oxsum-server --test capacity --run-ignored only --no-capture
