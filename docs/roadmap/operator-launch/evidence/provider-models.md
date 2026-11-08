# Business model and routing editor evidence

Issue [#818](https://github.com/AllureCurtain/oxsum/issues/818), child of
N04/#730. Implementation `fff4640` on main `45a833e`. Development source,
PostgreSQL authority fixture and frontend-build evidence; browser J03 and
candidate acceptance remain pending. No model or payment request was sent.

## Implemented workflow

The business channel detail now exposes model creation and complete immutable
version append, relative route weight, full price history, alias creation and
confirmed removal, ordered fallback editing and response-cache configuration.
Catalog and routing capabilities control entry points; connection destinations
and write-only secrets retain their separate owner capability. Existing scoped
business endpoints enforce every operation. The API contract is unchanged.

The model editor retains base and conditional rates, optional absent versus
zero values, supplier cost including an explicitly empty object, and exact
service-tier names. Independent review caught whitespace normalization in a
tier name; the correction preserves the original text and a weight-only-edit
regression now covers it. Base channel billing rates are described as legacy
customer fallback, separately from supplier cost and published sale terms.
Money uses checked decimal-to-BigInt minor units. Unsafe stored integers refuse
the edit. Mode changes require reviewing each output rate/limit, with no hidden
reset of conditional fields.

Fallback reads must complete before the form mounts. Closed queries retain no
stale cached chain; background focus reads do not replace an open draft.
Unknown write outcomes keep their draft and key for unchanged retry. Editing
creates a new intent. A list refresh or later permission refusal does not
silently discard an open draft; the backend still enforces current authority.
Cache expiry is a positive duration or an explicit no-expiry choice, and the
deprecated system-exclusion setting is not exposed as a working control.

## Executed validation

- Four new Node checks passed for complete price/cost/rule round trips, safe
  integer and flat-fee limits, explicit mode review, ordered fallback and cache
  expiry. The three existing money/sale checks also passed.
- All four OpenAPI credential-contract checks passed; code generation produced
  no schema change. TypeScript and Vite production build passed. Vite retains
  its existing large-chunk advisory; it is not a build failure.
- The real PostgreSQL HTTP `business_providers` test passed, exercising every
  existing provider operation, current authority, changed/unchanged retries,
  wrong ownership and denied effects. It remains a server fixture, not a UI test.
- Strict workspace Clippy with warnings denied passed in 47.16 seconds. Pinned
  formatting, diff and locale parsing checks passed.
- Independent source review found no remaining blocker after the exact-tier
  preservation correction. It covered capability separation, optional/cost
  fields, retry identity and explicit fallback/cache semantics.

Sanitized build, HTTP and Clippy logs are retained under ignored
`local-notes/operator-launch/repair-validation/provider-models-*.log`.

## Unperformed acceptance

Current `ego-browser listTaskSpaces()` returned only unrelated user-owned
spaces 0 and 1; the original task space 4 remains absent. The applicable browser
skill prohibits replacing a task space to recover or claiming a user-owned
space without explicit direction. No unrelated page was operated and no new
space was created. Desktop/mobile screenshots, keyboard/focus behavior, both
locale renderings and browser error/retry/reload checks are therefore pending.
The code includes both locale resources and shared accessible components, but
source inspection cannot certify those browser behaviors. J03 and final
candidate verification remain open after software integration.
