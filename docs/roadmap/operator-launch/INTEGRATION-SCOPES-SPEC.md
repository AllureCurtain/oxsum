# GUARDRAIL SPEC — #785 Scoped application provisioning credentials (N21.a)

Branch `feat/integration-scopes`, base `main` @ `0314cbd`. Issue: https://github.com/AllureCurtain/oxsum/issues/785 (child of #747 N21).

## Mission

Extend `IntegrationScope` beyond `customersRead`/`meteringWrite` so an application can act under a dedicated, narrowly-bounded grant — the credential foundation N21's embedded-app integration needs. Contract-first: confirm the exact scope names in `crates/server/openapi.yaml` before implementing.

## Scope mechanics (already in place — read before changing)

- `crates/core/src/integration_credentials.rs`: `IntegrationScope` enum (`as_str()` ↔ `from_row` string match). `require_integration(tx, service, scope, customer)` re-checks current expiry + revocation + scope + customer ownership **on every call** — `FOR SHARE` row lock so revocation can't commit mid-authorization.
- `crates/core/src/customers.rs` and `crates/core/src/metering.rs` show the call pattern: `require_integration(&mut tx, service, IntegrationScope::X, Some(customer))`.
- Mint path: `POST /api/v1/business/integration-credentials` → `business::create_integration_credential` → `create_integration_credential` (`IntegrationsManage` capability, owner-ish). `customer_ids` allowlist stored in `oxsum.integration_credential_customers`.
- Scopes persist as `text[]` — no migration needed to *store* a new enum variant, but the openapi `IntegrationScope` schema and the core enum must agree.

## Proposed scope set (confirm contract-first)

`customersProvision`, `keysManage`, `entitlementRead`, `checkoutInitiate`, `billingRead`, `evidenceRead` — reconcile against N21's acceptance and the existing read surface; drop/rename any that don't map to a real, enforceable operation this PR actually adds.

## Hard rules

- **Do not fabricate authority.** Only add a scope if this PR also wires it into a real enforcement point (a `require_integration` call guarding a real operation). A scope that nothing enforces is worse than none — it reads as granted authority that isn't checked. If a capability has no operation yet (e.g. provisioning endpoint doesn't exist), either add the minimal operation too, or leave that scope out and say why in the evidence.
- Namespace bounding: provisioning authority is confined to accounts the application itself created under its grant. Do NOT let it implicitly cover every existing business customer. The `customer_ids` allowlist keeps scoping existing-customer reads.
- A credential with only these scopes must not self-credit, edit a historical bill, read a provider/merchant secret, or grant staff membership — prove each denial.
- Contract-first: update `crates/server/openapi.yaml` `IntegrationScope` enum + any request/response schema before the core change.
- English only; integer minor units; `scripts/fmt.sh`; `cargo clippy --workspace --all-targets` clean; `cargo nextest run` on touched suites.

## Deliverables

- Core: new `IntegrationScope` variants wired to real `require_integration` enforcement points.
- Tests (`crates/core/tests/` + `crates/server/tests/`): each scope grants exactly its operation; cross-scope denial; revoked/expired fails closed; unknown scope string refuses; namespace bounding keeps foreign customers out.
- `docs/roadmap/operator-launch/evidence/integration-scopes.md` + `TODO.md` entry.
- Commit split: contract+core / tests / docs. Do NOT push or open a PR — report back.
