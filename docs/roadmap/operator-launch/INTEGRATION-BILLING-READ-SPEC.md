# GUARDRAIL SPEC — #788 Scoped application billing and balance reads (N21.b)

Branch `feat/integration-billing-read`, rebased onto `main` @ `03f21e9` after #785 / PR #786 merged (its `0105_integration_provision` migration and `customersProvision` scope are now in the base). Issue: https://github.com/AllureCurtain/oxsum/issues/788 (child of #747 N21, follows #785 / PR #786).

## Mission

Give the embedded application a way to read **its own authorized customers'** balance and bill state through a service credential — the read half of N21's "application displays balance/entitlement, payment and bill state" requirement and a J18 dependency.

Contract-first: confirm the exact scope name + request/response schema in `crates/server/openapi.yaml` before implementing.

## Design (decided)

The customer-facing balance/bills read resolves a per-customer `Wallet` via `organizations.tenant_id` → `state.tenants.get(&tenant_id)`; that resolution is **server-side**. The authorization (scope + customer grant) is **core-side** and must stay fail-closed there. So the split is:

- **Core** adds `IntegrationScope::BillingRead` and `Db::integration_customer_tenant(&ActingService, customer_id) -> Result<String>`: open tx → `require_integration(.., BillingRead, Some(customer_id))` → `SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1` → return the tenant id. A customer outside the grant returns `NotFound` from `require_integration`; an org without a row returns `NotFound`.
- **Server** adds a service-only route that: extracts `ActingService` via `integration(state, &headers)`, calls `integration_customer_tenant`, opens `state.tenants.get(&tenant_id)`, and reads the **same primitives** the `Principal` `balance`/`bills` routes use — `w.available()`, `w.credit_limit()`, `w.credit_used()`, `w.reserved()`, `w.month_spend()`, `w.transactions_page(before, limit)`.

This never loosens the existing `Principal`-only routes; the service gets its own surface.

## Route + schema (contract-first)

- `GET /api/v1/business/customers/{customerId}/billing` — service-only (bearer `IntegrationCredential`).
  - Response `{balance: BalanceRes-shape, bills: {rows, nextCursor}}` reusing the exact field names of `BalanceRes`/`BillRowRes` (`availableMinor`, `creditLimitMinor`, `creditUsedMinor`, `reservedMinor`, `spentThisMonthMinor`; `index`,`entryId`,`bookedOn`,`kind`,`amountMinor`,`description`,`contentHash`). Optional `?before=<u64>` pages the bills log backwards; optional `?limit` bounded 1..=50 (default 20) since a service may page.
- `IntegrationScope` enum gains `billingRead`.

## Files

- `crates/server/openapi.yaml` — `billingRead` in `IntegrationScope` enum; `scopes` `maxItems` 3→4; new route + `CustomerBilling` schema. (`customerIds` keeps the `minItems: 0` #785 added for provision-only grants; `billingRead` still requires an explicit grant, enforced in core.)
- `crates/core/migrations/0106_integration_billing_read.sql` — DROP/ADD `integration_credentials_scopes_check`: cardinality 1–4, adds `'billingRead'` to the set `0105` left (`customersRead`, `meteringWrite`, `customersProvision`). Register `"0106_integration_billing_read"` in `crates/core/src/db.rs`.
- `crates/core/src/integration_credentials.rs` — `BillingRead` variant + `as_str`/`from_row` + `Db::integration_customer_tenant`.
- `crates/server/src/routes/business.rs` — `customer_billing` handler.
- `crates/server/src/routes/mod.rs` — `GET /business/customers/{customer_id}/billing` → `customer_billing`.
- Tests in `crates/core/tests/` + `crates/server/tests/`.
- `docs/roadmap/operator-launch/evidence/integration-billing-read.md` + `TODO.md`.

## Hard rules

- **No fabricated authority.** Only `billingRead` is added — it is wired into a real `require_integration` enforcement point on the new operation. Do NOT add `keysManage`/`evidenceRead`/`checkoutInitiate`/`entitlementRead` here.
- **Namespace bounding.** `require_integration(.., Some(customer))` already confines the read to `integration_credential_customers` grantees. Prove a foreign customer and a different-credential's customer both fail.
- **No privilege bleed.** A `billingRead`-only credential must not provision, meter, manage keys, initiate checkout, self-credit, edit a bill, or read provider/merchant secrets — test each denial.
- `billingRead` is read-only — never on a write path.
- Contract-first; English; integer minor units; `scripts/fmt.sh`; `cargo clippy --workspace --all-targets` clean; `cargo nextest run` on touched suites.

## Deliverables

Commit split: contract+core+migration / tests / docs+evidence. Do NOT push or open a PR — report back.
