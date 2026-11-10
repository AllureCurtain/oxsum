# Separate-store application development acceptance (#942)

Date: 2026-11-10, Asia/Shanghai. Parent #747 / #725, N21 / J18.
Branch: `feat/embedded-application`; prerequisite join `407dcf8e` combines
#941 (`27bea59f`) and #940 (`8f2e24cf`). This is an unmerged development stack,
not a release candidate. PostgreSQL 17 and owned loopback HTTP model/payment
stubs run locally; the consumer is a separate Python process with private SQLite.
No real PSP/model account, independent pilot or external receiver was used.

## Observed behavior

- Python signup maps two local customers into one explicitly granted application.
  Exact repeat survives new consumer processes; changed customer/input identity
  refuses. Runtime receives no owner cookie, password or admin credential.
- A bounded customer consumption key is retained privately. Pending checkout has
  no funds and a paid call is refused. An actual application quote/order reaches
  the local merchant stub, then a labeled verified-event fixture and existing
  fulfillment move exactly 1,000,000 credit minor to the wallet.
- One real gateway turn through the model stub costs 40 minor. The consumer drops
  its successful response, restarts, and retries the exact retained operation.
  The server returns Idempotent-Replayed, the wallet is 999,960, reservations are
  zero, and exactly one 40-minor usage record exists. Changed replay is refused.
- Separate process/store boundaries, wrong customer order, wrong namespace/grant,
  revoked credentials and archive denial are observed. Archive preserves history
  and produces signed notifications delivered to a separate Python/SQLite inbox;
  that process reads current archived entitlement before marking reconciliation.
- A real assigned-plan application checkout, local verified payment and ordinary
  fulfillment produce a paid statement. The consumer discovers/reads it and
  publishes/downloads complete original-byte evidence through scoped endpoints.
- The actual receipt and statement exports are checked with native Rust and a
  clean installed verify-oxsum-bill tarball. Offline JavaScript binds retained
  receipt hash, catalog signatures, customer/manifest digest and exact signed
  origin/key. Nine tamper/wrong-scope/missing-trust cases reject. Downloading or
  retaining a hash is explicitly not an external witness or PSP attestation.

## Validation

- Joined focused application checkout/statement/notification/evidence/identity
  selection: **46 passed**, 10.590 seconds, zero skipped.
- `scripts/check-embedded-application.sh` builds WASM, packs/installs a clean
  temporary consumer and invokes the real-listener fixture. Latest packed run:
  **1 passed**, 4.172 seconds, with both real exports verified and nine negative
  cases rejected. The script uses no registry publication or live payment.
- Python runtime tests: **7 passed**, including two competing processes sharing
  the file lock, exact restore/retry, durable three-attempt and 23-hour gateway
  fences, absent one-time secret recovery, private key output, frozen pending
  checkout, independent trust/store binding, lossless integer bytes and local
  checkout HTML escaping/unsafe URL refusal. Receiver tests: **6 passed**, including retained-inbox upgrade and a full
  failed batch yielding to a later customer while failed reads remain pending.
- Five OpenAPI checks, generated types, ten frontend grant/link tests and joined
  production frontend build passed. Existing bundle-size warning remains.
- One full joined workspace run: **2,228 passed**, zero failed, six existing
  opt-ins skipped; test execution 609.132 seconds. The final receiver uses six
  passing Python checks; the packed focused listener rerun covers test-only child
  cleanup and current notification reconciliation.
- Strict core/server all-target Clippy passed (5m37s), as did pinned formatting.
  Workflow actionlint, shell
  syntax, Python compilation and diff whitespace checks passed. Python checks
  are also included in fast CI.

Earlier fixture corrections used the real Hash parser, a valid merchant slug,
Hyperswitch's explicit HE_02 absent response and stable borrowed trust strings.
They did not relax payment/network/proof production guards. All test PSP success
signals are labeled fixtures; no actual PSP-delivered event is claimed.

## Practical limits and remaining gates

The local CLI assumes authenticated local control; it is not a public user-login
service. The app store contains private customer keys, checkout handles and
prompts/results; it is fixed to one deployment/application and needs protected
backups. Unknown inference stops after three persisted attempts or 23 hours;
no retry after server idempotency expiry is automatically authorized. Successful
cached operations are historical receipts, while reads recheck current authority.
Key creation cannot recover lost one-time plaintext; revoke and explicitly replace.

Current read windows are bounded. Independent origin/key inputs are operator/pilot
responsibility; this fixture supplies its known test key out of band. The local
checkout page is tested at source/helper level but has no observed SDK browser
acceptance. Browser/J18 against the actual candidate image, independent review,
protected integration and external provider/PSP/pilot/reference-capacity gates
remain. A full passing development suite does not close N18 or certify N21's
candidate acceptance.

## Submission

Submitted as draft PR #943 (`f9ed9593`) against #941, with #940 joined as
documented above. Fast CI [42730958099](https://github.com/AllureCurtain/oxsum/actions/runs/42730958099)
passed all executed jobs, including required fmt/clippy/lib-doc/wasm/frontend.
Heavy integration jobs are deliberately skipped on PRs; the local full run above
is separate evidence. Independent approval/protected integration remain pending.
