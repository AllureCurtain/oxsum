# Local Hyperswitch sidecar

A self-hosted [Hyperswitch](https://github.com/juspay/hyperswitch) switch for
the deposit rail (issue #319). The rail's `.env` keys (`OXSUM_HYPERSWITCH_*`)
come from the merchant this stack seeds.

## One-time setup

The database migrations live in the upstream repository, not here — the
`migration_runner` service mounts a clone of it. Fetch one the first time:

```bash
./infra/hyperswitch/setup.sh            # clones into infra/hyperswitch/.repo
```

That directory is gitignored; it is a cache, not a vendor. To upgrade the
switch, pull it and re-copy `config/` and `scripts/` from the same tag.

## Run

```bash
docker compose -f infra/hyperswitch/compose.yml up -d
```

The first boot takes several minutes: migrations run, then the router, the SDK
bundle server, the Control Center, and `create-default-user` which seeds the
`demo@hyperswitch.com` / `Hyperswitch@123` merchant.

- Router (the API the rail calls): http://localhost:8080
- SDK bundle server: http://localhost:9050
- Control Center: http://localhost:9000 — sign in with the demo user above

## Wiring the rail

1. In the Control Center, the seeded merchant already has a Stripe test
   connector only if `create-default-user` configured one — check Connectors
   and add one with your `sk_test_…` key if the list is empty.
2. Read the three values the rail needs:
   - `api-key`: Developers → API Keys (create one, `snd_…`)
   - `payment_response_hash_key`: Settings → Payment Settings
   - `publishable_key`: Developers → API Keys (`pk_snd_…`)
3. Put them in `.env` beside `OXSUM_HYPERSWITCH_BASE_URL=http://localhost:8080`.
4. Webhooks: point the Control Center's webhook endpoint at
   `<public-url>/api/v1/webhooks/hyperswitch` (a tunnel in local dev — see
   docs/development.md).

## Notes on the copy

`compose.yml`, `config/` and `scripts/` were copied from `juspay/hyperswitch`
main and trimmed: monitoring / scheduler / Kafka / ClickHouse / OpenSearch
profiles removed, Postgres pinned to 17, ports moved off oxsum's (5433/6380),
`superposition-init` runs under `bash` not `sh`, and `fiuu.third_base_url` is
stubbed because the router refuses to boot without it. `diesel.toml`'s
`patch_file` is commented out — the patch upstream ships does not apply to the
schema its own branch generates, and `print-schema` is a codegen aid the
runtime does not need.
