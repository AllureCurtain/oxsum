# Scoped application evidence publication

Issue #934; `feat/application-evidence-publish` based on #933 (`de4149ae`),
migration 0127. Local PostgreSQL and HTTP fixtures only. Implementation validation completed.

Explicit application-only `evidenceWrite` durably records one exact operation,
customer, credential and period/statement selection under current authority.
It does not upgrade evidenceRead, grant sharing or authorize financial actions.
The original deterministic evidence input/marker/descriptor sequence recovers
interruptions without rewriting captures or signed bytes. Credentials and account
state are checked before acceptance; current authority is rechecked before the
successful response. Accepted work is not retroactively withdrawn by revocation.
No authority connection remains held during nested Wallet acquisitions.

Application bounds: at most 31 ended UTC days, 1,000 artifacts, 256 referenced
entry links, 4 MiB serialized collector and 16 MiB final bundle. Direct source
scans use a limit plus overflow refusal, not truncation; variable source values
are measured within the same PostgreSQL snapshot before transfer. Nested
statement/journal/effect groups cap at 256 rows; headers are measured too.
Immutable entry/posting/dimension groups cap at 4,096 rows and 4 MiB each
before proof generation. Existing private-cost captures
refuse unchanged. Final output reuses the existing original-byte integrity and
disclosure check; a supplied operator key still does not establish independent
consumer trust.

Executed checks:

- Eight original core publication/read cases passed (1.707s).
- Nineteen combined core/HTTP and ordinary evidence checks passed (3.942s), with
  one existing opt-in skipped.
- Eleven final focused publication/read cases passed (2.056s), covering concurrent
  replay, credential rotation, one-connection initial/retry operation, exact
  original bytes, real signed plan statement, foreign/scope/archive/expiry,
  changed replay, private scope collisions, failed descriptor recovery and
  actual collection overflow before marker creation.
- Nine frontend grant/fragment checks, four OpenAPI checks, generated types and
  production build passed; existing large-bundle warning remains.
- Complete workspace execution ran 2,187 cases in 340.137s: 2,186 passed,
  one migration table-inventory assertion failed solely on alphabetical ordering,
  and six existing opt-ins were skipped. The inventory order was corrected.
- Final focused execution passed all 46 selected core/HTTP/evidence/migration
  cases (8.137s); 220 nonselected/opt-in cases were excluded by the explicit filter.
  This includes the corrected inventory, late revocation after durable acceptance,
  archive before acceptance and actual oversized adjustment/statement header
  refusal before transfer or frozen input. Four new cases postdate the full run;
  all 2,191 ordinary cases now have passing full/focused evidence.
- Strict core/server all-target Clippy and final pinned formatting passed.
  Unchanged full-run passes are reused under the repository validation policy.

Independent review, protected integration, browser and exact candidate acceptance
remain pending. No provider/payment attempt occurred. Named statement checkout,
signed lifecycle notifications and the complete external application remain N21.
