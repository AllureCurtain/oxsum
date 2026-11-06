#!/usr/bin/env bash
# oxsum backup (issue #619): one custom-format dump plus a manifest that says
# exactly what the dump holds — the schema version it was taken at, the
# organizations and ledger schemas inside, and a SHA-256 over the file, so a
# restore or an audit can verify the artifact before it is trusted.
#
#   scripts/backup.sh [output-dir]
#
# Reads DATABASE_URL (or the compose trio POSTGRES_USER/POSTGRES_PASSWORD/
# POSTGRES_DB against a postgres service) from .env when present. Output is
# oxsum-backup-<utc>-<sha8>.dump beside a manifest of the same basename.
#
# WARNING: the dump contains live secrets — sealed upstream credentials and
# every tenant's ledger. Encrypt at rest; treat it like the database itself.

set -euo pipefail

OUT_DIR="${1:-.}"
# Load .env for the operator's own values; real env vars win.
if [ -f .env ]; then
    set -a; # shellcheck disable=SC1091
    . ./.env; set +a
fi

PGUSER="${PGUSER:-${POSTGRES_USER:-postgres}}"

# Where the database lives: a container (CONTAINER=name) or psql/pg_dump on
# PATH pointed at DATABASE_URL. The compose deployment's postgres service is
# the default when it answers; CONTAINER="" forces local tools.
CONTAINER="${CONTAINER:-}"
if [ -z "$CONTAINER" ] && command -v docker >/dev/null 2>&1; then
    if docker ps --format '{{.Names}}' 2>/dev/null | grep -qx "oxsum-core-postgres-1"; then
        CONTAINER="oxsum-core-postgres-1"
    elif docker ps --format '{{.Names}}' 2>/dev/null | grep -qx "oxsum-postgres-1"; then
        CONTAINER="oxsum-postgres-1"
    fi
fi

run() {
    if [ -n "$CONTAINER" ]; then docker exec "$CONTAINER" "$@"; else "$@"; fi
}
run_sh() {
    if [ -n "$CONTAINER" ]; then docker exec "$CONTAINER" sh -c "$1"; else sh -c "$1"; fi
}
scalar() { run psql -U "$PGUSER" -d "${POSTGRES_DB:-oxsum}" -tAc "$1" | tr -d '[:space:]'; }

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
base="oxsum-backup-${stamp}"
mkdir -p "$OUT_DIR"
dump_inside="/tmp/${base}.dump"
dump_out="${OUT_DIR%/}/${base}.dump"
manifest_out="${OUT_DIR%/}/${base}.manifest.json"

echo "dumping to $dump_out"
if [ -n "$CONTAINER" ]; then
    run_sh "pg_dump -U '$PGUSER' -Fc -d '${POSTGRES_DB:-oxsum}' -f '$dump_inside'"
    docker cp "$CONTAINER:$dump_inside" "$dump_out"
    run_sh "rm -f '$dump_inside'"
else
    run pg_dump -Fc -d "${DATABASE_URL:-postgres://$PGUSER@127.0.0.1:5432/${POSTGRES_DB:-oxsum}}" -f "$dump_out"
fi

size="$(wc -c < "$dump_out" | tr -d '[:space:]')"
if [ "${size:-0}" -lt 1024 ]; then
    echo "the dump is empty or truncated: $size bytes" >&2
    rm -f "$dump_out"
    exit 1
fi

sha="$(shasum -a 256 "$dump_out" 2>/dev/null || sha256sum "$dump_out")"
sha="${sha%% *}"

schema_version="$(scalar "SELECT coalesce((SELECT max(version) FROM oxsum._migrations), 'unmigrated')" || echo unknown)"
organizations="$(scalar "SELECT count(*) FROM oxsum.organizations" || echo 0)"
ledger_schemas="$(scalar "SELECT count(*) FROM pg_namespace WHERE nspname LIKE 'ledger_%'" || echo 0)"
postgres_version="$(scalar "SHOW server_version" || echo unknown)"

cat > "$manifest_out" <<EOF
{
  "artifact": "$dump_out",
  "createdUtc": "$stamp",
  "postgresVersion": "$postgres_version",
  "schemaVersion": "$schema_version",
  "organizations": $organizations,
  "ledgerSchemas": $ledger_schemas,
  "sizeBytes": $size,
  "sha256": "$sha"
}
EOF

echo "dump:       $dump_out ($size bytes, sha256 $sha)"
echo "manifest:   $manifest_out"
echo "contents:   $organizations organizations, $ledger_schemas ledger schemas, schema $schema_version"
echo
echo "the dump contains live secrets (sealed credentials, every ledger) — encrypt at rest."
