-- Drop per-test scratch databases left behind by interrupted runs.
--
-- Two families accumulate on the dev cluster:
--   oxsum_test_<slug>_<unix_timestamp>_<pid>   (oxsum-testkit per-suite DBs)
--   de_<32 lowercase hex>                       (doubleentry Postgres harness)
--
-- The suites drop their own databases at process exit, but a killed or timed-out
-- run never reaches that code. Run this any time the dev cluster feels heavy;
-- the pattern is strict enough that a real database cannot match it.
--
--   psql "$DATABASE_URL" -d postgres -f scripts/drop-test-scratch.sql
--   docker exec -i oxsum-postgres-1 psql -U postgres -d postgres -f - < scripts/drop-test-scratch.sql

SELECT format('DROP DATABASE %I WITH (FORCE);', datname)
FROM pg_database
WHERE datname ~ '^(de_[0-9a-f]{32}|oxsum_test_[a-z0-9_]+_[0-9]+_[0-9]+)$'
ORDER BY datname
\gexec
