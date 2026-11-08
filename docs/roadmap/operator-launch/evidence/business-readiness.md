# Business setup readiness (#843)

The old business checklist inferred identity from staff count, required extra
staff and a sold plan, and treated any merchant row as collection readiness.
`GET /api/v1/business/readiness` now checks five configuration facts under the
current personal Business session. The portal has separate loading, failed,
restricted, incomplete and ready states. No deployment token is needed.

The solo pay-as-you-go path requires the initialized owner name, a supported
priced chat route with available decryptable credentials, matching active
default sale terms, an explicitly selected enabled checkout route with compatible
collection terms and runtime prerequisites, and an invitation/registration path.
Extra staff, plans and an existing customer are optional. Role-restricted facts
never make the aggregate ready and do not expose their configuration values.

Development validation on integrated checkout selection `23b6547`:

- Twenty core tests passed with zero skips: the new complete solo scenario and
  provider, sale-book, merchant-route and staff suites. The scenario covers
  publication versus activation, unrelated models, expired/future schedules,
  sold aliases, disabled/cooling credentials, an unreadable secondary candidate,
  missing/wrong process key, unselected/disabled merchants, runtime return URL,
  role projection, customer context, revoked membership and no fake completion
  mutation. Fixture endpoints are inert example domains; no request is made.
- Five HTTP tests passed with zero skips: the new session/machine/deployment
  token boundary and existing provider/sale/staff/checkout transport suites.
- Strict workspace Clippy with all targets and `-D warnings` passed, as did
  formatting, four OpenAPI security checks, generated types and frontend build.
- Independent parent source review found no blocking issue. Full workspace
  regression on `d0ea03ca2b23ae4444e83f5e0681f41b0b8fb3fb` passed all
  1,950 executed tests across 191 binaries: build 2m 05s, execution 287.275s.
  Six existing opt-ins remain skipped: two real-provider tests, the gateway
  load and sustained-capacity checks, and two fixture/vector regeneration
  helpers. The worktree used an empty ignored `.env` and explicit local
  PostgreSQL URLs for both oxsum and the engine suites.
- PR #845's standard checks passed on that same implementation SHA in
  [run 42501845505](https://github.com/AllureCurtain/oxsum/actions/runs/42501845505).
  Entire's external trail gate reports missing reviewer approval; its CI,
  findings and up-to-date subchecks passed. Normal PR integration is pending.

The task worktree is `.worktrees/business-readiness` on
`feat/business-readiness`. Local logs are `/tmp/oxsum-843-core-focused.log`,
`/tmp/oxsum-843-http-focused.log`, `/tmp/oxsum-843-clippy.log` and
`/tmp/oxsum-843-workspace.log`.

This is configuration evidence, not external provider/PSP certification, SMTP
delivery, browser acceptance or final-candidate acceptance. The previous browser
task space is unavailable; no unrelated browser space was used. N09 still needs
customer model/key/funding readiness, fully entitled zero-wallet use, immediate
real bill proof and the complete J02/J08 journeys. Preparation found actual
fully-funded/free-call admission defects tracked by #849; customer readiness
must follow that correction rather than claiming an unavailable journey ready.
