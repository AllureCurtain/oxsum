# Scoped application billing and balance reads (#788, N21.b)

Child of #747 (N21), following #785 / PR #786. Branch `feat/integration-billing-read`, base `main` @ `81c1500`.

## What shipped

- `IntegrationScope::BillingRead` — a read-only scope for a granted customer's balance and bill log.
- Service-only route `GET /api/v1/business/customers/{organizationId}/billing` returning `{balance, bills}` — the same fields the customer `GET /balance` and `GET /bills` surfaces carry (`availableMinor`, `creditLimitMinor`, `creditUsedMinor`, `reservedMinor`, `spentThisMonthMinor`; `index`,`entryId`,`bookedOn`,`kind`,`amountMinor`,`description`,`contentHash`, `nextCursor`). `?before=<index>` and `?limit=<1..50>` page the bill log backwards.
- `Db::integration_customer_tenant(&ActingService, customer_id)` runs `require_integration(.., BillingRead, Some(customer))` `FOR SHARE` (rechecks expiry, revocation, scope and grant on every call), then returns the org's `tenant_id` for the server to open the customer's `Wallet`. Authorization stays inside core; wallet resolution stays server-side, mirroring the `Principal` path.
- Migration `0106_integration_billing_read` widens `integration_credentials_scopes_check` to cardinality 1–4 and the scope set `{customersRead, meteringWrite, customersProvision, billingRead}` (it carries `customersProvision` forward from `0105`).

## Scope decision

`billingRead` is added alone. #785 deferred `keysManage`, `entitlementRead`, `checkoutInitiate` and `evidenceRead` because the surfaces they would guard still had no `ActingService` enforcement path; this change adds the first such path on a customer billing read, and only `billingRead` maps to the operation actually shipped. The other scopes stay deferred until their operations exist — adding them here would fabricate authority nothing checks.

## Namespace bounding

`require_integration(.., Some(customer))` confines the read to customers in `integration_credential_customers` — the credential's explicit grant. It does not implicitly cover every business customer. When #786 merges, provisioned customers land in the same grant table, so the billing read's namespace unifies with the provisioning namespace without rework.

## Denials proven

- A `billingRead` credential reading a customer outside its grant → 404 (core `NotFound`).
- A different credential's granted customer → 404.
- An unknown customer id → 404.
- A `customersRead`, `meteringWrite` or `customersProvision` credential calling the billing read → 403 (core `Forbidden`), and the same over HTTP — provision mints accounts but cannot read their money.
- A revoked `billingRead` credential → `InvalidCredentials` (fail closed).
- An expired `billingRead` credential → `InvalidCredentials`.
- A forged/unknown `oxs-int-` secret → 401.
- A personal session cookie on the service-only route → 401.

`billingRead` appears on no write path: it cannot provision, meter, manage keys, initiate checkout, self-credit or edit a bill, and it grants no access to provider or merchant secrets — there is no read surface for those.

## Validation

- `cargo nextest run -p oxsum-core --test integration_billing_read`: 4/4 pass (`billing_read_resolves_the_granted_customers_tenant`, `billing_read_denies_ungranted_and_foreign_customers`, `other_scopes_cannot_read_billing`, `revoked_and_expired_billing_read_fail_closed`).
- `cargo nextest run -p oxsum-server --test integration_billing_read`: 2/2 pass (`billing_read_serves_the_granted_customers_balance_and_bills` asserts the seeded 7,500 top-up surfaces as `availableMinor` + a `topUp` row; `wrong_scope_cannot_read_billing`).
- `cargo check -p oxsum-server -p oxsum-core`: clean.
- `scripts/fmt.sh`, `cargo clippy --workspace --all-targets`: see the closing report.

No real model or payment spend attempted.
