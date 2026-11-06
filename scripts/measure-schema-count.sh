#!/usr/bin/env bash
# Per-tenant schema overhead (issue #618, T27): the D09 1,000-customer target
# keeps each customer's ledger in its own schema — `ledger_<tenant>` — so the
# per-tenant cost is the tables+indexes the engine creates, not a shared
# table's row growth. This script counts those catalog rows per tenant schema
# and reports the totals an operator can project from.
#
# Usage: measure-schema-count.sh [database-url]
#   Defaults to $DATABASE_URL. When `psql` is not on PATH, the script falls
#   back to the compose container (`docker exec oxsum-postgres-1`) — the
#   documented local database.

set -euo pipefail

URL="${1:-${DATABASE_URL:-}}"

# A psql invocation: the binary when present, else the compose container's.
if command -v psql >/dev/null 2>&1 && [ -n "$URL" ]; then
    psql_cmd() { psql "$URL" -v ON_ERROR_STOP=1 --no-psqlrc --tuples-only; }
elif docker ps --format '{{.Names}}' 2>/dev/null | grep -q '^oxsum-postgres-1$'; then
    psql_cmd() {
        docker exec -i oxsum-postgres-1 \
            psql -U postgres -d "${PGDATABASE:-oxsum}" \
            -v ON_ERROR_STOP=1 --no-psqlrc --tuples-only
    }
else
    echo "measure-schema-count: no psql on PATH and no oxsum-postgres-1 container" >&2
    exit 2
fi

# The engine names every tenant schema `ledger_<tenant>`; the oxsum-owned tables
# live in `oxsum`. Per-schema the count is the tables + indexes `migrate`
# wrote — the number a 1,000-tenant deployment multiplies.
psql_cmd <<'SQL'
SELECT
    n.nspname AS schema,
    count(*) FILTER (WHERE c.relkind = 'r') AS tables,
    count(*) FILTER (WHERE c.relkind = 'i') AS indexes,
    count(*) FILTER (WHERE c.relkind = 'S') AS sequences,
    pg_size_pretty(COALESCE(SUM(pg_total_relation_size(c.oid)) FILTER (WHERE c.relkind = 'r'), 0)) AS heap_size
FROM pg_namespace n
LEFT JOIN pg_class c ON c.relnamespace = n.oid AND c.relkind IN ('r','i','S')
WHERE n.nspname LIKE 'ledger\_%' OR n.nspname = 'oxsum'
GROUP BY n.nspname
ORDER BY n.nspname;
SQL

echo
echo "aggregate:"
psql_cmd <<'SQL'
SELECT
    count(DISTINCT n.oid) FILTER (WHERE n.nspname LIKE 'ledger\_%') AS tenant_schemas,
    count(c.oid) FILTER (WHERE n.nspname LIKE 'ledger\_%' AND c.relkind = 'r') AS tenant_tables,
    count(c.oid) FILTER (WHERE n.nspname LIKE 'ledger\_%' AND c.relkind = 'i') AS tenant_indexes
FROM pg_namespace n
LEFT JOIN pg_class c ON c.relnamespace = n.oid AND c.relkind IN ('r','i')
WHERE n.nspname LIKE 'ledger\_%' OR n.nspname = 'oxsum';
SQL
