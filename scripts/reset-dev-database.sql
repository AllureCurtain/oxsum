-- Reset the local development database to an empty state: every tenant
-- `ledger_*` schema is dropped and every mutable `oxsum` table is
-- truncated. The `oxsum._migrations` registry is preserved, so the schema
-- version stays at whatever `cargo build` / the last migration run left.
--
-- Development and test databases only. Integration tests share the
-- development database and leave behind an organization, a user and a
-- tenant ledger per fixture, which accumulates across runs; this is the
-- documented way to clear that residue after `cargo test --workspace`.
-- Never run it against an environment holding data that matters.
--
--   psql "$DATABASE_URL" -f scripts/reset-dev-database.sql
--
-- `\gexec` executes each generated DROP/TRUNCATE as its own statement, so
-- every schema drop commits separately; wrapping thousands of drops in one
-- transaction exhausts `max_locks_per_transaction`.

\echo 'Dropping tenant ledger schemas'

SELECT format('DROP SCHEMA %I CASCADE;', schema_name)
FROM information_schema.schemata
WHERE schema_name LIKE 'ledger\_%' ESCAPE '\'
\gexec

\echo 'Truncating oxsum tables'

SELECT format('TRUNCATE oxsum.%I RESTART IDENTITY CASCADE;', table_name)
FROM information_schema.tables
WHERE table_schema = 'oxsum' AND table_name <> '_migrations'
ORDER BY table_name
\gexec

\echo 'Development database reset'
