# Stable application accounts (#846)

An owner creates an immutable bounded application namespace and issues explicit
scopes for that application. New service-managed customers retain their existing
organization/ledger identity and separate immutable application external ID.
Provisioning creates no person, membership or money, and never adopts an existing
portal account. Archived accounts retain capacity and financial history.

Operation receipts bind the exact normalized request and result in the stable
application namespace. Credential rotation preserves mapping, provisioning and
metering replay. Metering admission retains the original credential and service
name for ledger provenance and billing descriptions while completion rechecks the
current replacement's application/scope. Legacy grants retain separate identity;
legacy provisioning only recovers historical operations when the original actor,
request, target and external identity agree. An allowlist is not ownership proof.

Migration 0114 follows integrated daily accounting 0113. Rebase onto `b3eb063a`
preserved production/test patches; conflicts were documentation additions. The
first type check exposed unsupported digest hexadecimal formatting; byte-wise
encoding corrected it without changing request identity. The final registered
runtime is `5420b171`.

Executed local validation with PostgreSQL and an empty worktree `.env`:

- Core application accounts, integration credentials, metering and migration
  identity: 39 passed, zero skipped, in 7.599 seconds.
- HTTP application accounts, integration credentials and metering: 13 passed,
  zero skipped, in 3.973 seconds; includes the maintained Python consumer.
- Contract generation/security (four checks), TypeScript and Vite build passed.
- Strict workspace/all-target Clippy passed with `-D warnings` in 1m51s;
  formatting and diff checks passed.
- Independent source review passed before the digest compilation correction;
  final review and combined full workspace validation pending.

Regressions cover cross-application identical IDs, scopes/revocation, concurrent
exact retries, changed terms, retained capacity, one-way archive with in-flight
settlement, atomic rollback, actual original metering provenance after rotation,
and exact historical provision recovery. Archived/suspended state is fetched
through GET; an operation retry returns its retained receipt.

The owner portal is the focused follow-up #853. Verified human consent linking,
scoped gateway key/entitlement/checkout/evidence operations, signed notifications
and the complete separate external application remain under N21/#747. No browser,
external provider/payment, candidate or complete J18 acceptance is claimed here.
