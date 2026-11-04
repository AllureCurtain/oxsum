# Source reconciliation

Reviewed 2026-11-05 at `02d25373021a920b1df5c7f5bf4eea1549a081ec`. Main was clean, with no task worktrees or open PRs. The only open issue was #506 (provider-account OAuth import), outside this release. Main was refreshed with `git pull --ff-only`; no commits or changes were discarded.

## Decisions confirmed by source

| Source | Observation | Consequence |
| --- | --- | --- |
| `crates/core/src/orgs.rs`, `sessions.rs`, `keys.rs`, `tenants.rs` | Organization UUID determines the tenant; memberships and keys resolve to it; Tenants caches one wallet per tenant over a shared pool | D01 preserves Organization as Customer. Business is a seller profile/authority scope with no second sale wallet |
| `crates/server/src/routes/billing.rs`, `routes/mod.rs` | Customer principals can call raw topups, holds and settlements | T01 removes self-credit and arbitrary settlement authority; gateway and verified deposits remain |
| `crates/server/src/admin/mod.rs`, `auth.rs`, `metering.rs` | Admin requires deployment token; metering requires a separate service credential | Retain initial trust boundaries; T03 adds staff capabilities; T05 narrows service/customer scope |
| `crates/core/src/cache.rs` | Cache key includes organization/channel/model/prompt digest | Preserve customer isolation; T07 verifies full authorization/prompt context |
| `crates/core/src/channels.rs`, `billing.rs` | Immutable channel prices and pure itemized rating exist | Reuse arithmetic/history; T08 adds independent sale books and T09 authenticates terms |
| `crates/doubleentry/src/storage/postgres.rs` | Append begins/commits its own transaction; internal validation is centralized | T11 prototypes caller-transaction integration with full rollback/MMR/limit tests |
| `crates/core/src/subscriptions.rs` | Quota draw and timer rollover exist independently of wallet postings | Preserve old period history; T19 requires authorization for each new paid period |
| `crates/core/src/deposits.rs`, `crates/server/src/admin/ops.rs` | Deposits, reversals and provider refund status exist | T15/T17 add durable purpose fulfillment and bounded pending refunds; local reversal is not PSP success |
| `.github/workflows/ci.yml` | PR fast tier; heavy nextest/process/engine checks run after merge | Root AGENTS.md's full required-chain claim is stale. Run relevant local integration before risky merges; T29 aligns candidate policy |
| `.github/workflows/release-image.yml`, `deploy.yml` | Release image builds/smokes without publishing; Deploy publishes and restarts a live host | Never dispatch Deploy as validation; T29 separates publication and deployment |

These are source observations, not newly executed financial, migration, browser or capacity acceptance. Existing features are foundations, not automatic completion of T01–T31.

## Initial route ownership

Paths are relative to `/api/v1` unless stated. T05 completes executable per-route coverage, including exports and auxiliary surfaces.

| Surface | Baseline principal | Required release authority |
| --- | --- | --- |
| POST topups, holds, settlements | Customer session/key | Retire unsafe mutations. Manual receipts require finance/deployment recovery authority, reason/reference and audit; usage injection uses scoped metering |
| Rail topups, redemptions, subscription checkout | Customer session/key | Initiate payment or redeem operator-issued value; never assert paid state; immutable order purpose |
| Payment webhooks | Provider signature | Configured merchant context plus signature/order/amount/currency/customer checks |
| Admin adjustments, credit notes/limits, deposit reverse, statement payments | Deployment token | Protected initially; business finance capability and source-linked audit in later tasks |
| Admin channels, routing, plans, credentials | Deployment token | Business configuration capability; owner-only secrets unless explicitly delegated |
| Metering | Service credential | Deny consumption keys/cookies; T05 adds scopes and customer allowlists |
| `/v1/*`, tasks/cache/fallback/experiments | Consumption key | Allowed models/channels for pinned customer, bounded admission, no foreign payer selection |
| Org, session switch, membership/keys | Customer principal with existing role checks | Retain membership identities; T05 narrows consumption-key management; staff/customer roles independent |
| Balance, entries/proofs, bills/export, usage, reports/links, statements/PDF, websocket | Customer scope | Inspect every read/export/share; receipt access cannot grant customer-wide data |
| Public verification/key and bill links | Public/share capability | Honest proof scope and share lifecycle; exclude supplier costs/secrets |
| `/metrics`, recovery/control | Deployment token | Deployment authority, never customer credential |

## Compatibility

Preserve IDs, balances, postings, origins and signed proofs. Display currency is not economic conversion. Map global configuration to one business without promoting existing customer owners. A03 must prove this on an old fixture including key resolution and signed roots.

Portfolio-only positioning and whole-organization key authority are superseded for the release target. Unsafe money privileges have no permanent compatibility bypass. Target documents are not claims of shipped behavior; each implementation PR updates current behavior documentation.

Plan money and entitlement remain distinct. Provider expense belongs to the operator; there is no project wholesale charge. Each paid order fulfills one purpose. Reports distinguish rated value, earned usage, promotional value, allowance, collections and debt. D10 supplies the journal acceptance map.

## Baseline gates

CI run [41830423690](https://github.com/AllureCurtain/oxsum/actions/runs/41830423690) and image run [41830423648](https://github.com/AllureCurtain/oxsum/actions/runs/41830423648) completed successfully for the baseline SHA. They are historical baseline evidence, not validation of this release. Exact candidate gates, built UI, verifier packaging, capacity, restore, merchant certification and pilots remain pending.
