#!/usr/bin/env bash
# Backup and restore drill (issue #326).
#
# Dumps the source database with pg_dump's custom format, restores it into a
# fresh database on the same cluster, and compares the counts that prove the
# move: organizations, statements, deposits, usage records, and the ledger
# schemas the tenant ledgers live in. Exits non-zero on any mismatch, so the
# drill can run in a pipeline as well as by hand.
#
# The defaults are the local compose deployment (container oxsum-postgres-1,
# database oxsum, user postgres). A deployment with psql/pg_dump/pg_restore on
# PATH and no Docker can set CONTAINER="" to run them directly — see the
# "Backup and restore drill" section of docs/development.md.
#
# The restored copy is a *second* database on the same cluster: the drill never
# touches the source. After it passes, start the server against the restored
# copy (development.md prints the command) and verify a bill before treating
# the backup as good.

set -euo pipefail

CONTAINER="${CONTAINER-oxsum-postgres-1}"
SOURCE="${SOURCE:-oxsum}"
TARGET="${TARGET:-oxsum_drill_restore}"
DUMP="${DUMP:-/tmp/oxsum-backup.dump}"
PGUSER="${PGUSER:-postgres}"

if [ "$SOURCE" = "$TARGET" ]; then
    echo "SOURCE and TARGET must differ: the drill never restores over its source" >&2
    exit 2
fi

# Run one command in the cluster, or locally when CONTAINER is empty.
run() {
    if [ -n "$CONTAINER" ]; then
        docker exec "$CONTAINER" "$@"
    else
        "$@"
    fi
}

# Run a shell line inside the container, or locally when CONTAINER is empty.
# Paths that only exist inside the container (the dump) travel in this string
# rather than as arguments: Git Bash on Windows rewrites a `/tmp/...` argument
# into `C:/...` before docker ever sees it.
run_sh() {
    if [ -n "$CONTAINER" ]; then
        docker exec "$CONTAINER" sh -c "$1"
    else
        sh -c "$1"
    fi
}

# One scalar out of one database.
scalar() {
    run psql -U "$PGUSER" -d "$1" -tAc "$2" | tr -d '[:space:]'
}

echo "dumping $SOURCE to $DUMP"
run_sh "pg_dump -U '$PGUSER' -Fc -d '$SOURCE' -f '$DUMP'"

# A zero-byte dump is the failure mode a pipe hides; check the file before the
# restore rather than after it.
size="$(run_sh "wc -c < '$DUMP'" | tr -d '[:space:]')"
if [ "${size:-0}" -lt 1024 ]; then
    echo "the dump is empty or truncated: $size bytes" >&2
    exit 1
fi
echo "dump size: $size bytes"

echo "restoring into $TARGET"
run dropdb -U "$PGUSER" --if-exists "$TARGET"
run createdb -U "$PGUSER" "$TARGET"
run_sh "pg_restore -U '$PGUSER' -d '$TARGET' '$DUMP'"

# The counts that prove the move. A ledger schema per organization means the
# schema count is the one that catches a restore that stopped early.
checks=(
    "organizations|SELECT count(*) FROM oxsum.organizations"
    "statements|SELECT count(*) FROM oxsum.statements"
    "deposits|SELECT count(*) FROM oxsum.deposits"
    "usage records|SELECT count(*) FROM oxsum.usage_records"
    "ledger schemas|SELECT count(*) FROM information_schema.schemata WHERE schema_name LIKE 'ledger\\_%'"
)

failed=0
printf '%-16s %12s %12s\n' "check" "source" "restored"
for check in "${checks[@]}"; do
    label="${check%%|*}"
    sql="${check#*|}"
    before="$(scalar "$SOURCE" "$sql")"
    after="$(scalar "$TARGET" "$sql")"
    mark=""
    if [ "$before" != "$after" ]; then
        mark="  MISMATCH"
        failed=1
    fi
    printf '%-16s %12s %12s%s\n' "$label" "$before" "$after" "$mark"
done

if [ "$failed" -ne 0 ]; then
    echo "the restored copy does not match the source" >&2
    exit 1
fi

cat <<EOF
the restored copy matches the source.

Next: start a server against it and verify a bill before trusting the backup.
  DATABASE_URL=postgres://$PGUSER@127.0.0.1:5432/$TARGET OXSUM_ADDR=127.0.0.1:3012 \\
      cargo run -p oxsum-server
  curl -fsS http://127.0.0.1:3012/healthz
EOF
