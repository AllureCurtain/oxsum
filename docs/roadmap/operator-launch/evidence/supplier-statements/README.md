# Scoped OpenAI supplier statement acceptance (#1237)

Engineering checks use synthetic fixtures, local PostgreSQL and the real portal.
No authentic OpenAI statement, admin credential or provider account mapping was
available. A successful fixture comparison is not real-provider certification.

## Contract and focused checks

The implemented format is the actual OpenAI `GET /v1/organization/costs`
contract, checked against the [OpenAI-owned OpenAPI specification](https://github.com/openai/openai-openapi/blob/fa361566ec236d24fad012d24d8aec352cab0cc0/openapi.json).
It requires numeric `amount.value` lexemes; the importer never converts them
through a binary float. One full UTC day, explicit organization/project scope,
query/cursor chain and operator mapping/conversion assertions are retained.

- `supplier-parser-tests2.log`: 6 passed, including exact signed decimal/exponent
  parsing, aggregation before truncation, malformed/missing data, scope, UTC
  buckets and page termination, plus schema inventory.
- `supplier-legacy-cost-tests.log`: 7 passed for disabled unsafe OpenAI automatic
  attribution and strict legacy decimal handling. The old job preserves existing
  OpenAI observations; it does not overwrite them with unknown.
- `supplier-focused-final.log`: 17 passed after final concurrency hardening:
  five parser/unit tests, seven PostgreSQL suites and five server/job suites.
  Coverage includes concurrent source versions and retry, live authority,
  canonical hashes, exact replay after later comparisons, UTC attempt cohorts,
  unknown historical routes, partial missing-attempt diagnostics, cache rows,
  conversion residual/overflow rollback, more than 50 comparisons, selected
  immutable exports, finance success and customer/key/viewer refusal.
- `supplier-clippy-final.log`: strict core/integrations/server Clippy passed.
- `supplier-codegen-final.log`, `supplier-frontend-final.log`, `supplier-fmt.log`:
  OpenAPI/locale contracts, generated types, TypeScript/Vite and pinned formatting
  passed. Local execution logs are retained in `/tmp/oxsum-billing-program/`.

Full workspace: **2642 passed, 6 skipped**, 526.303 seconds
(`supplier-workspace.log`, `cargo nextest run --workspace --no-fail-fast`).

## Browser acceptance

Ego Chromium exercised the real React portal and rebuilt server with migration
0143 against `oxsum_correction_browser`. A dummy canonical OpenAI channel and
synthetic official-format USD 0.06 file were used, with explicit fixture mapping
and conversion. No provider request was sent. The UI uploaded original file text,
reviewed scope/amount and imported source version 1; the result showed known
estimate 0, difference 0.06 and incomplete coverage. See [preview](preview.png)
and [selected frozen comparison](comparison.png).

Import and comparison intents survived reload and retried successfully. In the
post-commit response-loss case, the browser awaited a successful comparison
response, discarded it deliberately, then reloaded. **Retry exact saved request**
left the database at one source and three comparisons, with no duplicate. See
[retained intent](retry.png). Selecting the original comparison and downloading
it again produced a byte-identical file to the earlier export; both source and
comparison canonical SHA-256 digests were independently recalculated successfully.
The known local cost is zero only for this empty synthetic UTC cohort; the UI
still explicitly refuses to infer complete provider coverage.

At 390px, the document width was 390px, and the comparison/unknown-coverage/export
content remained readable. All four screenshots were visually inspected; see
[mobile](mobile.png). These checks establish a functioning portal, not authentic
supplier reconciliation. Source revisions, large comparison history and finance
permissions are additionally exercised by the database/HTTP suites above.

## Independent review and external evidence

Independent agent `/root/review_recovery_supplier` approved the final code with
no remaining actionable findings. It inspected snapshot concurrency and
permission revocation, append-only source/comparison identity, exact replay,
canonical export, cost/coverage boundaries, concrete API contract and UI
preview/retry/history behavior. It also read the actual final focused and Clippy
logs. It did not claim browser, CI or provider execution.

[The pilot protocol](../../billing-workflow-pilots.md) specifies authentic Costs
pages/query/cursors, account/project-to-channel mapping, cost-unit conversion,
caller-authorized stored-completion recovery and two independent operator cases.
These remain external #740/#745 inputs. No production deployment or candidate
promotion is part of this work.
