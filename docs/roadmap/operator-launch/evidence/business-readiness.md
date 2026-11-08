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
- Independent parent source review found no blocking issue. Complete workspace
  regression and protected PR checks are still pending at this checkpoint.

The task worktree is `.worktrees/business-readiness` on
`feat/business-readiness`. Local logs are `/tmp/oxsum-843-core-focused.log`,
`/tmp/oxsum-843-http-focused.log` and `/tmp/oxsum-843-clippy.log`.

This is configuration evidence, not external provider/PSP certification, SMTP
delivery, browser acceptance or final-candidate acceptance. The previous browser
task space is unavailable; no unrelated browser space was used. N09 still needs
customer model/key/funding readiness, fully entitled zero-wallet use, immediate
real bill proof and the complete J02/J08 journeys.
