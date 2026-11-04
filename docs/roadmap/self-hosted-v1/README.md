# Self-hosted release contract

Adopted specification: [#591](https://github.com/AllureCurtain/oxsum/issues/591), 2026-11-05. Source baseline: `02d25373021a920b1df5c7f5bf4eea1549a081ec`.

oxsum is self-hosted operating software for a business selling AI service to individual and team customers. The operator owns provider access, sale prices and merchant collection. The oxsum project receives no payment or percentage. Release one supports one business and many customer accounts per deployment.

Existing organizations remain customer billing accounts. Their IDs, tenant schemas, balances, keys, statement identities, ledger origins and historical proofs stay intact. A separate Business owns seller configuration and staff authority. Customer ownership never implies business authority.

This contract specifies the implementation target; adoption does not mean the features or release gates are complete. [STATUS.md](STATUS.md) records actual issue/PR/check evidence.

## Reading order

1. [PRODUCT.md](PRODUCT.md): R01–R12, actors, capabilities and commercial defaults.
2. [DESIGN.md](DESIGN.md): D01–D10, persistence, transactions, payment/plan state machines and journals.
3. [BASELINE.md](BASELINE.md): source reconciliation, compatibility and initial route ownership.
4. [TASKS.md](TASKS.md): dependency-ordered implementation and completion criteria.
5. [ACCEPTANCE.md](ACCEPTANCE.md): A01–A31 and promotion gates G1–G5.
6. [STATUS.md](STATUS.md): issue mapping and next action; [evidence/](evidence/) holds performed checks.

## Execution rules

Follow root and nested AGENTS.md. One focused issue and PR per feature; split large cards into dependent child issues. Contract changes precede implementation. Use worktrees from current main. Preserve unrelated work and never commit on main or bypass CI. Since baseline heavy integration checks run after merge, perform corresponding relevant isolated checks before risky merges; T29 aligns candidate CI policy.

Update status and evidence after every task and before handoff. Record exact SHA, command, environment, result and remaining prerequisites. Resume from actual git/PR state; do not infer completion from existing names or reset pending work. The local execution package's progress.json is a machine-readable mirror; this tracked directory remains sufficient to understand requirements and progress.

## External boundaries

Prepare software and reproducible candidate artifacts without real-money calls, live deployment/data migration, publication or pilot contact. Those actions require separate authorization for the concrete candidate. The existing Deploy workflow publishes an image and restarts a live server: do not dispatch it during implementation.

Merchant certification, real-provider paid calls and two independent operators remain external gates when unavailable. Finish independent software work and record those gates as pending; unit tests or a feature inventory cannot establish release readiness.
