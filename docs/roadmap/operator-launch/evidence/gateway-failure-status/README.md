# Accurate gateway response status after admission

Issue #1024. Provider rotation acceptance #1023 on main `2ee70601` observed
HTTP 502 at the client and `httpStatus: 200` in the corresponding errored request
row. The gateway settled every admitted turn through a log initializer that
hard-coded 200, even before a response was selected.

The relay plan now retains an optional selected gateway response status. The
pipeline sets it before each whole-response settlement and before handing an
SSE body to the client. Upstream refusal/unreachable/body-read/invalid-JSON and
whole/cache guardrail refusals retain their returned error status; successful
whole/cache/SSE responses retain 200. Cancellation before selecting any response
leaves the field absent. An already-started SSE retains 200 on later body failure
or cancellation. Accounting decisions, immutable history and existing log rows
are unchanged. The OpenAPI description and generated types explain this scope.

## Validation

- The disabled-credential regression failed on the original source with actual
  200 versus expected 502, after a real scratch-database setup.
- All 133 selected gateway, request-log, guardrail and cache tests passed after
  the repair. These are affected suites in the server aggregate test binary,
  not a workspace or final-candidate claim.
- Adding cancellation-before-response coverage was followed by all eight final
  request-log cases passing. Existing guardrail checks cover fresh/cached refusal
  403 and already-streamed policy handling 200, under both wallet and quota funding.
- New cases cover disabled credentials for whole/SSE requests, upstream refusal
  (the gateway returns 502 independently of the provider's 418), malformed JSON,
  broken whole body, broken already-started SSE and cancellation before headers.
- Strict `cargo clippy -p oxsum-server --all-targets -- -D warnings`, pinned
  formatting, five contract checks, generated schema and production frontend
  build passed. Unchanged broad coverage was reused after the final test-only edit.
- [listener-result.json](listener-result.json) is the original failing fixture
  re-exercised against the repaired ordinary binary over a real loopback listener.
  Client and request log both say 502, charge/attempts are zero, balance stays
  9,999,930 and no hold remains. The provider listener was absent, so this could
  not make a genuine model call. Source patch and binary hashes identify this
  uncommitted development artifact; the version's base SHA alone is not its identity.

The temporary listener was stopped and port 18821 checked absent. No browser
was opened for this HTTP regression. Original #1023 evidence remains unmodified;
this repair supplies new evidence instead of rewriting the failing observation.
Independent review, protected integration and final candidate acceptance remain.
