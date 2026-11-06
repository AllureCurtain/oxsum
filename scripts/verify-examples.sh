#!/usr/bin/env bash
# Fresh-install verification for the developer examples (issue #617).
#
# Boots a real oxsum deployment — a scratch PostgreSQL database plus the
# locally built server binary on a free port — seeds it, then runs every
# example end to end against it and counts each example's assertions. Any
# failure anywhere exits non-zero.
#
# Run from the repository root:
#   scripts/verify-examples.sh
#
# Requires: cargo (to build oxsum-server), node ≥ 20.16, a reachable
# PostgreSQL the same way the test suites need one (DATABASE_URL from .env,
# or docker compose up -d). The script never touches a deployment it did not
# start: it creates a scratch database, drops it on exit, and kills the
# server it spawned.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

BASE_DB="${DATABASE_URL:-postgres://postgres:postgres@127.0.0.1:5432/postgres}"
# The scratch database: one per run, dropped on exit like the test suites.
SCRATCH_DB="oxsum_examples_$(date +%s)_$$"
ADMIN_TOKEN="examples-admin-token-$(head -c 8 /dev/urandom | od -An -tx1 | tr -d ' \n')"
PORT="${OXSUM_EXAMPLES_PORT:-3987}"
BASE_URL="http://127.0.0.1:${PORT}"
SECRET_KEY="$(openssl rand -base64 32)"
HEAD_KEY="$(openssl rand -base64 32)"

DB_URL="$(python3 - <<EOF
from urllib.parse import urlsplit, urlunsplit
p = urlsplit("$BASE_DB")
print(urlunsplit((p.scheme, p.netloc, "/$SCRATCH_DB", "", "")))
EOF
)"

cleanup() {
    if [ -n "${SERVER_PID:-}" ]; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    python3 - "$BASE_DB" "$SCRATCH_DB" <<'EOF' 2>/dev/null || true
import sys, urllib.parse
base, name = sys.argv[1], sys.argv[2]
import subprocess
subprocess.run([
    "docker", "exec", "-i", "oxsum-postgres-1",
    "psql", "-U", "postgres", "-d", "postgres", "-c",
    f'DROP DATABASE IF EXISTS "{name}" WITH (FORCE);'
], capture_output=True)
EOF
}
trap cleanup EXIT

echo "== verify-examples: fresh deployment on ${BASE_URL} =="

# The scratch database: connect through the running postgres container when
# present, else psql on DATABASE_URL's host.
if docker ps --format '{{.Names}}' 2>/dev/null | grep -qx "oxsum-postgres-1"; then
    docker exec -i oxsum-postgres-1 psql -U postgres -d postgres -c \
        "CREATE DATABASE \"$SCRATCH_DB\";" >/dev/null
else
    PGPASSWORD="${PGPASSWORD:-postgres}" psql "$BASE_DB" -c \
        "CREATE DATABASE \"$SCRATCH_DB\";" >/dev/null
fi

echo "== Building oxsum-server =="
cargo build -p oxsum-server --bin oxsum 2>&1 | tail -3
BIN="$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')/debug/oxsum"
[ -x "$BIN" ] || { echo "server binary not found at $BIN" >&2; exit 1; }

echo "== Migrating the scratch database =="
DATABASE_URL="$DB_URL" "$BIN" --migrate-only

echo "== Seeding the demo world (gives a priced channel + owner) =="
DATABASE_URL="$DB_URL" \
OXSUM_DEMO_SEED=true \
OXSUM_SECRET_KEY="$SECRET_KEY" \
OXSUM_HEAD_SIGNING_KEY="$HEAD_KEY" \
OXSUM_ADMIN_TOKEN="$ADMIN_TOKEN" \
    "$BIN" seed

echo "== Starting the server =="
DATABASE_URL="$DB_URL" \
OXSUM_ADDR="127.0.0.1:${PORT}" \
OXSUM_ADMIN_TOKEN="$ADMIN_TOKEN" \
OXSUM_SECRET_KEY="$SECRET_KEY" \
OXSUM_HEAD_SIGNING_KEY="$HEAD_KEY" \
OXSUM_SESSION_COOKIE_SECURE=false \
    "$BIN" &
SERVER_PID=$!

for i in $(seq 1 60); do
    if curl -fsS "$BASE_URL/healthz" >/dev/null 2>&1; then break; fi
    sleep 1
    if [ "$i" = 60 ]; then echo "server did not come up" >&2; exit 1; fi
done
echo "server up."

CUSTOMER_ID=""
CUSTOMER_KEY=""

run() {
    local name="$1"; shift
    echo
    echo "== ${name} =="
    if "$@"; then
        echo "-- ${name}: OK"
    else
        echo "-- ${name}: FAILED" >&2
        exit 1
    fi
}

# The consumption walkthrough: invite → redeem → top up → gateway → usage →
# evidence. It needs the packed SDK; build and pack it the way the consumer
# check does. The tarball ships the wasm module out of `web/verify-sdk/wasm/`,
# so `build-verify-wasm.sh` must run first — a pack against the committed
# placeholders fails the wasm-present check the SDK's own packing asserts.
echo
echo "== Building the wasm module =="
bash scripts/build-verify-wasm.sh

echo
echo "== Packing verify-oxsum-bill =="
if [ ! -d web/verify-sdk/node_modules ]; then
    (cd web/verify-sdk && npm ci --no-audit --no-fund)
fi
WORK="$(mktemp -d "${TMPDIR:-/tmp}/oxsum-examples.XXXXXX")"
(cd web/verify-sdk && npm pack --pack-destination "$WORK")
TARBALL="$(find "$WORK" -maxdepth 1 -name 'verify-oxsum-bill-*.tgz' -print -quit)"
[ -n "$TARBALL" ] || { echo "npm pack produced no tarball" >&2; exit 1; }
(cd examples/consumption && npm install --no-audit --no-fund "$TARBALL" 2>&1 | tail -2)

# The walkthrough stands up its own loopback upstream and channel — the
# seeded demo channel deliberately targets an unreachable endpoint, so no
# OXSUM_MODEL is passed. Its last lines export the fresh customer (and the
# owner it registered when the business was new) for the later examples.
CONSUMPTION_LOG="$WORK/consumption.log"
if ! env \
    OXSUM_BASE_URL="$BASE_URL" \
    OXSUM_ADMIN_TOKEN="$ADMIN_TOKEN" \
    node examples/consumption/consumption.mjs | tee "$CONSUMPTION_LOG"; then
    echo "-- consumption walkthrough: FAILED" >&2; exit 1
fi
echo "-- consumption walkthrough: OK"

CUSTOMER_ID="$(grep '^OXSUM_CUSTOMER_ID=' "$CONSUMPTION_LOG" | tail -1 | cut -d= -f2-)"
CUSTOMER_KEY="$(grep '^OXSUM_CUSTOMER_KEY=' "$CONSUMPTION_LOG" | tail -1 | cut -d= -f2-)"
OWNER_EMAIL="$(grep '^OXSUM_OWNER_EMAIL=' "$CONSUMPTION_LOG" | tail -1 | cut -d= -f2-)"
OWNER_PASSWORD="$(grep '^OXSUM_OWNER_PASSWORD=' "$CONSUMPTION_LOG" | tail -1 | cut -d= -f2-)"
if [ -z "$CUSTOMER_ID" ] || [ -z "$CUSTOMER_KEY" ]; then
    echo "could not read back the walkthrough's customer" >&2; exit 1
fi
# When the walkthrough registered the business itself it prints the owner it
# created; on an initialized business the seeded demo owner is the fallback.
OWNER_EMAIL="${OWNER_EMAIL:-admin@oxsum.local}"
OWNER_PASSWORD="${OWNER_PASSWORD:-admin-password}"

run "scoped-management walkthrough" env \
    OXSUM_BASE_URL="$BASE_URL" \
    OXSUM_OWNER_EMAIL="$OWNER_EMAIL" \
    OXSUM_OWNER_PASSWORD="$OWNER_PASSWORD" \
    OXSUM_CUSTOMER_ID="$CUSTOMER_ID" \
    OXSUM_CUSTOMER_KEY="$CUSTOMER_KEY" \
    OXSUM_ADMIN_TOKEN="$ADMIN_TOKEN" \
    node examples/scoped-management/scoped-management.mjs

run "webhook sign-and-replay" env \
    OXSUM_BASE_URL="$BASE_URL" \
    OXSUM_OWNER_EMAIL="$OWNER_EMAIL" \
    OXSUM_OWNER_PASSWORD="$OWNER_PASSWORD" \
    node examples/webhook-tools/sign-and-replay.mjs

run "webhook dedup-check" env \
    OXSUM_BASE_URL="$BASE_URL" \
    OXSUM_OWNER_EMAIL="$OWNER_EMAIL" \
    OXSUM_OWNER_PASSWORD="$OWNER_PASSWORD" \
    node examples/webhook-tools/dedup-check.mjs

run "webhook retry-storm" env \
    OXSUM_BASE_URL="$BASE_URL" \
    OXSUM_OWNER_EMAIL="$OWNER_EMAIL" \
    OXSUM_OWNER_PASSWORD="$OWNER_PASSWORD" \
    node examples/webhook-tools/retry-storm.mjs --n 12

run "offline verify consumer" bash scripts/check-verify-consumer.sh

echo
echo "verify-examples: every example ran green against ${BASE_URL}."
