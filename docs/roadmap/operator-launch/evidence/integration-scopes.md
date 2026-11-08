# Scoped application provisioning credentials — development evidence

Issue: [#785](https://github.com/AllureCurtain/oxsum/issues/785) (child of #747, N21.a). Based on main `0314cbd`. Development evidence, not a merged candidate.

## Implemented behavior

`IntegrationScope` gains a third variant, `customersProvision`. An integration credential holding it may provision a service-managed customer billing account under the calling business via `POST /api/v1/business/customers` — a route that accepts only an `integrationCredential`, never a staff session.

The provisioned account is a real customer (`organizations` + `customer_profiles`) with no human login and no staff membership. Identity (`organization_id`, `business_id`, `external_id`) stays immutable — `customer_profiles_protect_identity` still refuses any UPDATE — so the placeholder profile the org-insert trigger creates is replaced by the caller's external identity in one delete+insert inside the same transaction. `UNIQUE(business_id, external_id)` is the namespace guard: a foreign `externalId` fails the insert.

The namespace is the credential's own grant: provisioning inserts the new account into `integration_credential_customers`, so the creating credential (and only it) can reach it. Provisioning does **not** grant `customersRead`/`meteringWrite` — the grant row is the read/meter allowlist source, so a provision-only credential can meter what it made once granted `meteringWrite`, and a different credential never reaches the account.

Idempotency is the `(business, externalId)` uniqueness of the profile: `business_mutations.actor_id` is a `users` FK a service credential cannot fill, so the durable dedup is the namespace key. An exact replay answers the standing customer.

A pure `customersProvision` credential mints with an **empty** `customerIds` allowlist and grows its own namespace; every other scope still requires 1–1000 explicit ids. Migration `0098` widens `integration_credentials_scopes_check` for the new variant and the 1–3 cardinality.

## Scope-set decision

The spec proposed `customersProvision`, `keysManage`, `entitlementRead`, `checkoutInitiate`, `billingRead`, `evidenceRead`. Only `customersProvision` was added: every other candidate would guard `Principal`-scoped customer surfaces (`keys`, `entitlement`, `billing`, `evidence` reads and checkout) that have **no `ActingService` enforcement path** — adding the enum variant would fabricate authority nothing checks. Per the no-fabricated-authority rule they are deferred until those surfaces gain a service credential path; `customersProvision` ships now because this PR adds the operation it guards.

## Denials proven

- `customersRead` and `meteringWrite` credentials cannot provision (403).
- A `customersProvision` credential cannot read or meter outside its grant; a different credential cannot reach the provisioned account (404).
- Revoked and expired credentials fail closed (`InvalidCredentials`).
- A provisioned account has no human membership (0 members) and no wallet credit.
- A session cookie cannot call `POST /business/customers` (service-only gate).

## Validation

- `cargo nextest run -p oxsum-core --test integration_provisioning` — **4/4 pass**
- `cargo nextest run -p oxsum-server --test integration_credentials` — **3/3 pass** (incl. new HTTP provision test)
- `scripts/fmt.sh`; `cargo clippy --workspace --all-targets` — see PR body for executed output
- Contract `crates/server/openapi.yaml` first: `customersProvision` added to `IntegrationScope`, `customerIds` `minItems` relaxed to 0 for provision-only grants, `POST /business/customers` + `ProvisionCustomerRequest` added.

## Remaining gates

The wider N21 lifecycle (key lifecycle, checkout initiation, billing/evidence reads, the external example app) is out of this child's scope and lands when the `ActingService` enforcement surface those scopes need exists. No real model/payment spend attempted.
