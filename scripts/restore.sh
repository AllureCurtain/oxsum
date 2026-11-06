#!/usr/bin/env bash
# oxsum restore (issue #619): a `scripts/backup.sh` dump into a *fresh*
# database, followed by `oxsum --migrate-only` so a dump taken on an older
# release is brought current before the serving binary ever sees it.
#
#   scripts/restore.sh <dump-or-manifest> <target-database-name>
#
# The target is created on the same cluster the compose postgres service runs
# (or, with CONTAINER="", wherever DATABASE_URL points); an existing target is
# refused — a restore over live data is the accident this script refuses to
# take part in.
#
# After the restore and migrate the script prints the verification queries a
# runbook checks: row counts and the schema digest. Restoring keys is not a
# separate step — credentials live inside the dump (sealed under
# OXSUM_SECRET_KEY, which the .env of the *restored* deployment must match).

set -euo pipefail

if [ $# -ne 2 ]; then
    echo "usage: $0 <backup.dump | backup.manifest.json> <target-db-name>" >&2
    exit 2
fi

INPUT="$1"
TARGET="$2"

# Load .env for POSTGRES_* / DATABASE_URL; real env vars win.
if [ -f .env ]; then
    set -a; # shellcheck disable=SC1091
    . ./.env; set +a
fi

PGUSER="${PGUSER:-${POSTGRES_USER:-postgres}}"
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
scalar_on() { run psql -U "$PGUSER" -d "$1" -tAc "$2" | tr -d '[:space:]'; }

# A manifest resolves to its dump and verifies the artifact before it is
# trusted; a bare .dump restores as-is.
DUMP="$INPUT"
expected_sha=""
if [[ "$INPUT" == *.manifest.json ]]; then
    DUMP="$(grep -oE '"artifact": *"[^"]+"' "$INPUT" | cut -d'"' -f4)"
    expected_sha="$(grep -oE '"sha256": *"[^"]+"' "$INPUT" | cut -d'"' -f4)"
    if [ ! -f "$DUMP" ]; then
        echo "manifest names $DUMP, which is not here — copy the dump beside the manifest" >&2
        exit 1
    fi
fi
if [ ! -f "$DUMP" ]; then
    echo "$DUMP does not exist" >&2
    exit 1
fi
if [ -n "$expected_sha" ]; then
    actual="$(shasum -a 256 "$DUMP" 2>/dev/null || sha256sum "$DUMP")"
    actual="${actual%% *}"
    if [ "$actual" != "$expected_sha" ]; then
        echo "sha256 mismatch: manifest says $expected_sha, file is $actual" >&2
        exit 1
    fi
    echo "artifact verified against its manifest"
fi

if [ "$TARGET" = "${POSTGRES_DB:-oxsum}" ]; then
    echo "refusing to restore over the live database ${TARGET}" >&2
    exit 2
fi
exists="$(run psql -U "$PGUSER" -d postgres -tAc "SELECT 1 FROM pg_database WHERE datname='$TARGET'" | tr -d '[:space:]')"
if [ "$exists" = "1" ]; then
    echo "refusing: database $TARGET already exists — restore only ever lands on a fresh database" >&2
    exit 2
fi

echo "creating $TARGET and restoring $DUMP"
run createdb -U "$PGUSER" "$TARGET"
if [ -n "$CONTAINER" ]; then
    docker cp "$DUMP" "$CONTAINER:/tmp/oxsum-restore.dump"
    docker exec "$CONTAINER" pg_restore -U "$PGUSER" -d "$TARGET" --no-owner --no-privileges /tmp/oxsum-restore.dump
    docker exec "$CONTAINER" rm -f /tmp/oxsum-restore.dump
else
    run pg_restore -U "$PGUSER" -d "$TARGET" --no-owner --no-privileges "$DUMP"
fi

echo
echo "restored. Now bring the schema current and verify:"
echo "  DATABASE_URL=postgres://$PGUSER:<password>@<host>:5432/$TARGET oxsum --migrate-only"
echo
echo "verification queries:"
echo "  SELECT max(version) FROM oxsum._migrations;          -- schema level the dump held"
echo "  SELECT count(*) FROM oxsum.organizations;            -- organizations restored"
echo "  SELECT count(*) FROM pg_namespace WHERE nspname LIKE 'ledger_%';"
echo
schema_version="$(scalar_on "$TARGET" "SELECT coalesce((SELECT max(version) FROM oxsum._migrations), 'unmigrated')" || echo unknown)"
orgs="$(scalar_on "$TARGET" "SELECT count(*) FROM oxsum.organizations" || echo 0)"
ledgers="$(scalar_on "$TARGET" "SELECT count(*) FROM pg_namespace WHERE nspname LIKE 'ledger_%'" || echo 0)"
echo "this restore holds: schema $schema_version, $orgs organizations, $ledgers ledger schemas"
