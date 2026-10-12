# Backend conventions

Global rules live in the root AGENTS.md; this file only covers backend-specific content.

## Layers

```
crates/server/src/routes/     Routing, request deserialization. No business logic here;
                              one module per resource group (auth, org, keys, billing,
                              usage, log, webhooks, device); mod.rs wires the router and
                              holds the helpers every module shares.
crates/server/src/admin.rs    The operator-token admin surface
crates/server/src/auth.rs     Auth middleware
crates/server/src/error.rs    WalletError → HTTP error code mapping
crates/core/src/              Business logic (Wallet, Tenants, proofs)
crates/doubleentry/           Ledger engine and storage
```

## Conventions

- Request bodies are deserialized with serde; fields are camelCase throughout. Business rules (positive amounts, tenant id format, etc.) are validated in core, not re-checked in the route layer.
- Errors are uniformly mapped to the format defined in docs/api.md. Storage-layer error details go to tracing logs only and never appear in responses.
- SQL must be parameterized. Schema names are only ever assembled by `Wallet::open` from a validated tenant id.
- Every write operation carries an `idempotencyKey`; the EntryId is derived deterministically from it.
- Every endpoint has at least one integration test in `crates/server/tests/`.
- The posting date is always the server's current UTC date; client-supplied dates are not accepted.
