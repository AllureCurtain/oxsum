# Current product claim audit

Date: 2026-11-08. N01/#727. Baseline source `0314cbd3d5439adbc904a327a8e16704b344a0af`.

This documentation change describes the checked-in implementation and labels
historical and future evidence separately. The stage's unmerged feature PRs are
not advertised as integrated. No provider call, external certification,
publication or final candidate acceptance was performed by this audit.

| Claim area | Source checked | Result in the documents |
| --- | --- | --- |
| Recommended operator entry | `docs/self-hosted-install.md`, `crates/server/src/routes/business_setup.rs`, owner setup and customer registration core | Empty deployment starts with an explicitly minted owner token; public customer registration grants neither staff nor free purchased credit; existing identities accept through `/api/v1/business/setup` |
| Business and customer authority | `business_staff.rs`, `integration_credentials.rs`, current OpenAPI/session context | Staff, customer-team roles, machine keys, deployment token and integration scope are distinct; embedded applications are part of the product |
| First funding and usage | Payment orders, manual receipts, current demo and readiness code | No stale customer self-credit/bootstrap credential recipe; demo repair and richer readiness remain tracked work until integrated; entitlement can permit zero-wallet usage |
| Entry verification | `crates/verify/src/bundle.rs`, WASM bill verifier, `/verify` fallback | `verify_bundle` checks hash/inclusion, not head signature; arithmetic can skip by schema; inspect price/sale status even when `ok` is true |
| Reports/statements/evidence | Usage, statement and evidence verifier modules and SDK | Digest/inclusion, optional signature, scope/completeness and per-row arithmetic are separate checks; unsigned input must not be described as authenticated |
| Retained trust and witness | Signed head/consistency/witness checks | A verifier needs independently retained keys/hashes/heads; another operator-owned process is not organizational independence |
| Integration coverage | Local server suites; T31 evidence and release checklist | Local protocol coverage is not blanket real-provider certification; historical Airwallex 403 and missing PSP credentials remain limitations |
| Release/distribution | Candidate workflows and SDK pack scripts | Main and PR fast checks do not establish a release candidate; SDK is locally packed unless publication evidence exists; exact full CI and artifact acceptance remain required |

Validation: `scripts/fmt-check.sh`, `git diff --check`, source-to-claim review and
relative Markdown target validation. `cargo clippy --workspace --all-targets -- -D warnings` passed before push.
Local-link validation checked all 15 changed documentation files with no missing
relative target. No runtime code changed;
the eventual integrated browser journeys remain N18 acceptance rather than a
documentation test claim.
