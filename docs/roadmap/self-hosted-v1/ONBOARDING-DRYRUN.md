# Onboarding dry run — operator to verified customer bill

Updated 2026-11-07. This walkthrough distinguishes image smoke, API setup,
automated domain/HTTP coverage and actual browser actions. It is not an
independent pilot or an end-to-end browser sale/renewal certification.

`scripts/release-candidate-smoke.sh` does **only** image config parsing,
fresh PostgreSQL migration, readiness/health/version SHA checks, owner setup
and login, business-context read, and issuance of one customer invitation.
It optionally runs the packed consumer; CI uses a separate mandatory
same-SHA consumer job. It does not redeem that invitation, configure a
provider/plan, make a call, take payment or complete steps 6–10 below.

| Step | Procedure | Executed evidence |
| --- | --- | --- |
| 0. Prerequisites | Docker/Compose, repository/deploy files; follow [install guide](../../self-hosted-install.md). | CI image smoke and PostgreSQL 17 integration fixtures. |
| 1. Configure | Copy `deploy/operator.env.example` to a private `deploy/.env`, set operator/database/sealing secrets, run `check-secrets.sh` and `check-config.sh`. | Image smoke generates its own private temp env/profile; it never replaces the operator's file. `install_ops.rs` covers validation. |
| 2. Migrate | Start postgres and run the `oxsum-migrate` compose service. | Image smoke fresh install; `install_ops.rs` fresh and previous-version fixture upgrade. |
| 3. Start | Start `oxsum`; probe `/readyz`, `/healthz`, `/api/v1/version`. | Image smoke asserts the response's full git SHA equals the requested candidate. |
| 4. Initialize | Mint an operator setup token, register the owner, login and explicitly choose business context. | Image smoke does these HTTP calls; browser closeout separately exercises owner login/context roundtrip. |
| 5. Add people | Owner issues a customer invitation. Customer redeems consent. Staff management appoints an **existing user UUID** to a role. | Smoke only issues the customer invitation. Browser closeout registers the customer using Tab/Enter and appoints finance/support through the UI after API-prepared identities. No staff invitation feature is claimed. |
| 6. Configure supply and sale | Configure authorized provider destination, prices and merchant; publish/bind a plan when selling one. | Browser fixture provisions channel/price/merchant through authenticated APIs. Plan lifecycle is covered by `plans.rs`, `entitlement.rs` and `plan_statements.rs`, not the browser journey. |
| 7. Purchase funding | Configure display currency, create wallet order, complete provider checkout and observe fulfilled credit. | Browser closeout uses a declared mock SDK/local provider, real order APIs, signed callbacks and the background fulfillment worker. The second order updates without reload. No real PSP moved money. |
| 8. Key and billed call | Create a scoped consumption key; make a billable request and inspect Requests/Bills. | Browser creates a key and runs Chat with its own page-minted key. The local stream reports six input/two output tokens, producing an actual eight-minor-unit settlement. |
| 9. Refund and statement | Customer requests refund; finance decides. For plans, close period/journal and inspect its immutable statement. | Browser finance declines one refund, approves another; signed local success yields a credit note and the customer's succeeded state. Plan statement close/payment is automated domain/HTTP coverage, not a browser action in this run. |
| 10. Verify | Export the authenticated proof/bundle, retain its hash or trusted head and verify independently. | Browser pastes the actual charged bill into `/verify`: genuine passes, wrong hash fails. The entry query parameter does not auto-fill the form. The packed consumer checks v2 fixtures offline and rejects mutations in its mandatory candidate job. |

The exact browser setup, screenshots, IDs, accounting reconciliation and
fixture source are retained in [closeout-browser.md](evidence/closeout-browser.md).
The candidate's run/artifact identities are in
[closeout-candidate.md](evidence/closeout-candidate.md). Neither record
substitutes internal automation for an unfamiliar operator's pilot.

For upgrades and restore, follow [the upgrade runbook](../../self-hosted-upgrade.md).
`install_ops.rs` checks previous-version migration and cross-database backup/
restore preservation. Use a separate restore destination and retain keys and
configuration; never run destructive rehearsal against a live database.
