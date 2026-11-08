# Explicit checkout routes (#835)

Owner-created merchants previously could not be selected by `POST /orders`:
only the environment bootstrap was consulted. Replayed checkout responses also
read current SDK metadata instead of the original order version.

Migration 0112 adds business/rail selections and immutable mutation receipts.
Owner writes require revision, reason and stable key; report readers see the
safe route metadata. Customers get only their business's enabled selected
routes. New order pinning rechecks selection/version under the business lock;
existing keys retain original versions, and changed rails conflict. Bootstrap
callback identity never changes and explicit disabled selections never fall
back. Concurrent identical creates return one winning order.

The UI exposes owner confirmation and customer test/live selection. Independent
review found that background route refresh could replace an unresolved payment
intent; choice snapshots and complete submitted payloads now remain fixed until
an explicit user edit. Recovery uses the original request even after offers or
collection configuration change. Native validation is bypassed only for that
already-submitted original intent so a withdrawn offer cannot block recovery.

Development evidence:

- 13 initial core checks passed (merchant configuration, order behavior and the
  new route scenario); an eight-way identical-create assertion was added before
  the final full run.
- 22 real PostgreSQL/HTTP checks passed, including a real loopback switch create,
  stale-choice refusal before I/O, secret-free customer options, signed authority
  boundaries, disabled-route replay and original SDK version. An initial new
  fixture omitted the switch's required read-before-create 404 JSON; the fixture
  was corrected and all 22 passed. No production guard was relaxed.
- OpenAPI checks, generated types and frontend build passed. Strict workspace
  Clippy passed. Full workspace run `b49304c4-2fca-49f2-a4f9-995595ce7c6d` on implementation
  `4328a87` passed **1,948/1,948**, with six existing opt-in skips, in 262.234
  seconds. The final native form-validation recovery fix changes only the UI;
  final frontend build and focused input checks are recorded separately.
- Independent core/UI review passed after the retained-intent and explicit
  confirmation fixes. Actual browser acceptance remains unperformed because the
  authorized browser task space is unavailable.

These tests perform no live payment or genuine external PSP certification.
