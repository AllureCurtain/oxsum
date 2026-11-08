# Customer evidence portal

Issue #863, under N12/#738. The existing customer evidence APIs now have a
portal route linked from Bills, customer navigation and actual plan statements.
Owners/admins explicitly freeze an ended period or retrieve/freeze an issued
statement. The page never mints on navigation or creates an external share.
Dates remain frozen across ambiguous-response retries; the existing server's
organization/span identity makes retry return the original bundle.

Downloads preserve the raw bundle JSON, including integer tokens larger than
JavaScript's safe range. Expected organization, ledger origin, bundle id and
manifest digest are selected separately; a second download retains them with the
available trust record. Verification uses the existing WASM evidence implementation
and explicitly authenticates the customer origin with verifySignedHead. The
current evidence WASM wrapper alone derives origin from its note, so a valid
signature from a different ledger must fail this additional check.

Executed development validation:

- Four behavior groups: valid ended UTC date spans, inclusive 366-day cap, malformed
  dates, customer/manifest substitution, positive local verdict, unsigned/malformed
  signature states and lossless large-integer/raw-envelope handling.
- Three actual-WASM groups: valid retained/supplied/unsigned evidence; changed
  amounts, missing committed artifacts, wrong customer/manifest/key; and a genuinely
  signed foreign origin. That last fixture passes the standalone wrapper but fails
  the portal's customer-origin check, demonstrating the added boundary.
- TypeScript and production Vite build passed; the existing large-chunk warning
  remains. API contract/codegen and formatting are included before submission.

These checks use the existing local verifier artifact and frozen engine-produced
fixtures. They do not constitute candidate-artifact, real-browser or offline
network-disabled acceptance. The initial manifest and key remain server observations
until separately retained/compared; unsigned records remain unsigned. No new
arithmetic, upstream honesty, supplier-book or independent-witness claim is made.
N12's candidate exports and rejection/compatibility matrix remain required.


Independent review identified and corrected two async state boundaries: statement
navigation remounts the customer-scoped view, so an old request cannot publish
into the new statement; verification checks that browser trust did not change
during the WASM load and freezes the exact trust snapshot in the verdict/export.
Final combined verification ran nine behavior tests and seven actual-WASM groups,
all passed, and the production build passed on integrated main `e53e303a`.
