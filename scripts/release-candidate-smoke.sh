#!/usr/bin/env bash
# Release-candidate smoke (issue #620, T29 / A30): bring up the self-hosted
# core profile against a THROWAWAY database and exercise the surfaces a gate
# needs to see alive — readiness, version, business initialization, one
# business read and one customer-surface write — then the offline verifier
# over genuine and tampered evidence.
#
# This script never touches a production deployment: it always creates its
# own compose project (PID-suffixed) and its own postgres volume, generates
# throwaway secrets it never prints, and tears everything down on exit
# including the volume. Run it from the repository root:
#
#   OXSUM_IMAGE_TAG=sha-<sha> scripts/release-candidate-smoke.sh
#
# OXSUM_IMAGE_TAG defaults to `candidate-local`; when that tag is missing
# locally and SMOKE_BUILD=1 (default) the script builds
# `ghcr.io/allurecurtain/oxsum:candidate-local` from this checkout — the
# compose files pin the ghcr namespace, so the tag is the only thing a
# smoke run varies.
#
# What it asserts, in order (each step fails loudly and names itself):
#   1. oxsum --check-config inside the image parses the smoke env
#   2. oxsum --migrate-only brings the schema current
#   3. /readyz answers 200, /healthz answers, /api/v1/version reports gitSha
#   4. business surface: setup-token mint (operator token) → owner register
#      → owner login → explicit business-context switch → GET /api/v1/business
#   5. customer surface: the staff-issued customer invitation
#      (`POST /api/v1/business/customer-invitations`) — always available and
#      the documented invite path, so the smoke does not depend on the
#      deployment's open-registration policy
#   6. the packed verify-oxsum-bill consumer verifies genuine evidence and
#      rejects the tampered fixtures (check-verify-consumer.sh — skipped
#      with SMOKE_SKIP_CONSUMER=1)
#
# Env: OXSUM_IMAGE (image under test), SMOKE_BUILD (default 1), SMOKE_PORT
# (host port, default 3917), SMOKE_SKIP_CONSUMER (default 0).

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

OXSUM_IMAGE_TAG="${OXSUM_IMAGE_TAG:-candidate-local}"
OXSUM_IMAGE="ghcr.io/allurecurtain/oxsum:${OXSUM_IMAGE_TAG}"
SMOKE_BUILD="${SMOKE_BUILD:-1}"
SMOKE_PORT="${SMOKE_PORT:-3917}"
SMOKE_SKIP_CONSUMER="${SMOKE_SKIP_CONSUMER:-0}"
PROJECT="oxsum-candidate-smoke-$$"
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/oxsum-candidate-smoke.XXXXXX")"
BASE="http://127.0.0.1:${SMOKE_PORT}"

step() { printf '\n== %s ==\n' "$*"; }
die() { echo "smoke FAILED at: $*" >&2; exit 1; }

# The compose services declare `env_file: [.env]`, which resolves beside the
# compose file — the env must live at deploy/.env while the project is up.
# The script writes the smoke file there and restores any pre-existing
# operator file on the way out; a deployment that already has one is left
# exactly as it was.
DEPLOY_ENV=deploy/.env
ENV_BACKUP=""
if [ -f "$DEPLOY_ENV" ]; then
    ENV_BACKUP="$WORKDIR/operator-env-backup"
    mv "$DEPLOY_ENV" "$ENV_BACKUP"
fi
cleanup() {
    (cd deploy && docker compose -p "$PROJECT" -f docker-compose.core.yml \
        down -v --remove-orphans >/dev/null 2>&1) || true
    if [ -n "$ENV_BACKUP" ] && [ -f "$ENV_BACKUP" ]; then
        mv "$ENV_BACKUP" "$DEPLOY_ENV"
    else
        rm -f "$DEPLOY_ENV"
    fi
    rm -rf "$WORKDIR"
}
trap cleanup EXIT

json_field() { sed -n "s/.*\"$2\":\"\([^\"]*\)\".*/\1/p" <<<"$1"; }

# --- image -----------------------------------------------------------------
step "candidate image: $OXSUM_IMAGE"
if ! docker image inspect "$OXSUM_IMAGE" >/dev/null 2>&1; then
    [ "$SMOKE_BUILD" = "1" ] || die "image $OXSUM_IMAGE missing and SMOKE_BUILD=0"
    echo "building $OXSUM_IMAGE from $(git rev-parse --short HEAD)…"
    docker build --build-arg "GIT_SHA=$(git rev-parse HEAD)" -t "$OXSUM_IMAGE" . \
        || die "docker build"
fi

# --- throwaway env ---------------------------------------------------------
POSTGRES_PASSWORD="$(openssl rand -hex 32)"
OXSUM_ADMIN_TOKEN="$(openssl rand -hex 24)"
OXSUM_SECRET_KEY="$(openssl rand -base64 32)"
OWNER_PASS="smoke-owner-$RANDOM$RANDOM"
cat > "$DEPLOY_ENV" <<EOF
POSTGRES_PASSWORD=${POSTGRES_PASSWORD}
OXSUM_ADMIN_TOKEN=${OXSUM_ADMIN_TOKEN}
OXSUM_SECRET_KEY=${OXSUM_SECRET_KEY}
OXSUM_BIND=127.0.0.1
OXSUM_PORT=${SMOKE_PORT}
OXSUM_SESSION_COOKIE_SECURE=false
OXSUM_BUSINESS_NAME=Candidate Smoke Co.
OXSUM_BUSINESS_SLUG=candidate-smoke
EOF
chmod 600 "$DEPLOY_ENV"

# Compose resolves `env_file: [.env]` and its own `--env-file` default beside
# the compose file — run it from deploy/ so both land on the smoke file.
compose() { (cd deploy && docker compose -p "$PROJECT" -f docker-compose.core.yml "$@"); }
export OXSUM_IMAGE_TAG

# --- 1. check-config -------------------------------------------------------
step "1. oxsum --check-config"
compose run --rm --no-deps oxsum --check-config || die "--check-config"

# --- 2. migrate ------------------------------------------------------------
step "2. postgres up + oxsum --migrate-only"
compose up -d postgres >/dev/null || die "postgres up"
compose run --rm oxsum-migrate || die "--migrate-only"

# --- 3. serve + probes -----------------------------------------------------
step "3. oxsum up, /readyz + /healthz + /api/v1/version"
compose up -d oxsum >/dev/null || die "oxsum up"
ready=""
for _ in $(seq 1 60); do
    if curl -fsS -o /dev/null "$BASE/readyz" 2>/dev/null; then ready=1; break; fi
    sleep 2
done
[ -n "$ready" ] || { compose logs oxsum | tail -30; die "/readyz never answered"; }
curl -fsS -o /dev/null "$BASE/healthz" || die "/healthz"
VERSION="$(curl -fsS "$BASE/api/v1/version")" || die "/api/v1/version"
echo "version: $VERSION"
grep -q '"gitSha"' <<<"$VERSION" || die "version payload lacks gitSha"

# --- 4. business surface ---------------------------------------------------
step "4. business init + owner session + business-context read"
SETUP="$(curl -fsS -X POST "$BASE/api/v1/admin/business/setup-tokens" \
    -H "authorization: Bearer ${OXSUM_ADMIN_TOKEN}" \
    -H "content-type: application/json" \
    -d "{\"idempotencyKey\":\"smoke-$RANDOM-$RANDOM\",\"ownerEmail\":\"owner@smoke.test\"}")" \
    || die "setup-token mint"
TOKEN="$(json_field "$SETUP" token)"
[ -n "$TOKEN" ] || die "setup-token response carried no token"
curl -fsS -X POST "$BASE/api/v1/business/setup/register" \
    -H "content-type: application/json" \
    -d "{\"token\":\"$TOKEN\",\"businessName\":\"Candidate Smoke Co.\",\"email\":\"owner@smoke.test\",\"password\":\"$OWNER_PASS\"}" \
    >/dev/null || die "owner register"
curl -fsS -X POST "$BASE/api/v1/auth/login" \
    -H "content-type: application/json" \
    -c "$WORKDIR/cookies" \
    -d "{\"email\":\"owner@smoke.test\",\"password\":\"$OWNER_PASS\"}" >/dev/null \
    || die "owner login"
# A fresh session is in customer context; the staff seat is claimed through
# the explicit context switch the console documents (#630).
CTX="$(curl -fsS "$BASE/api/v1/session/context" -b "$WORKDIR/cookies")" || die "session/context"
grep -q '"business":{' <<<"$CTX" || die "context carried no claimable business seat"
curl -fsS -X POST "$BASE/api/v1/session/context" -b "$WORKDIR/cookies" \
    -H "content-type: application/json" \
    -d "{\"idempotencyKey\":\"smoke-ctx-$RANDOM\",\"context\":\"business\"}" >/dev/null \
    || die "context switch"
BIZ="$(curl -fsS "$BASE/api/v1/business" -b "$WORKDIR/cookies")" || die "GET /business"
echo "business principal: $BIZ"

# --- 5. customer surface ---------------------------------------------------
step "5. staff-issued customer invitation"
curl -fsS -X POST "$BASE/api/v1/business/customer-invitations" \
    -b "$WORKDIR/cookies" -H "content-type: application/json" \
    -d "{\"idempotencyKey\":\"smoke-inv-$RANDOM\",\"email\":\"customer@smoke.test\",\"name\":\"Smoke Customer\",\"externalId\":\"smoke-cust-$RANDOM\",\"reason\":\"candidate smoke\"}" \
    >/dev/null || die "customer invitation"
echo "customer invitation issued (staff → customer surface exercised)"

# --- 6. offline verifier ---------------------------------------------------
if [ "$SMOKE_SKIP_CONSUMER" != "1" ]; then
    step "6. verify-oxsum-bill clean consumer (genuine + tampered)"
    bash scripts/check-verify-consumer.sh || die "check-verify-consumer"
else
    step "6. consumer check skipped (SMOKE_SKIP_CONSUMER=1)"
fi

step "SMOKE PASSED"
echo "image:        $OXSUM_IMAGE"
echo "version:      $VERSION"
echo "business:     owner registered, session switched, /api/v1/business answered"
echo "customer:     staff invitation issued"
[ "$SMOKE_SKIP_CONSUMER" = "1" ] || echo "verifier:     genuine accepted, tampered rejected"
echo "teardown:     compose project $PROJECT + volume removed"
