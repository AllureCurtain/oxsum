# Isolated developer examples (#769)

Development evidence, 2026-11-08. This is not final-candidate or real-provider
certification. The runner branch starts from `0314cbd`; its maintained example
source is PR #751 at `66f7dee8d05219ddf84b516d998a651df4d3a099`. That PR is an
integration prerequisite, explicitly selected with `--examples-root` during the
checks below. The default source on the old base fails closed.

## Executed checks

- Eight Python isolation checks pass: rejected provisioning overrides, sanitized
  environment, failed create with no fallback, exact same-connection cleanup,
  cleanup-error propagation, essential-skip rejection, source-copy immutability,
  and timeout/process cleanup with private output.
- Two complete actual PostgreSQL/listener walkthroughs pass. Consumption 10,
  event configuration 1, management 11, signed intake 8, deduplication 5,
  concurrent replay 6 and offline verification 11 assertions all pass. The only
  optional skip is the fixture's absent witness signature.
- The first attempted walkthrough found a fixture sale-window overlap. The
  corrected event setup applies an explicit model override alongside the
  consumption book, then both repeated runs pass. The failed attempt also
  stopped its listener and removed its own database.
- The second successful run freezes binary SHA-256
  `c41fbbdfd667f9e7924e8548050a474943fa85445cdb6359ec2cb1631e60a481`
  from the collection-authority development build and SDK SHA-256
  `ecc6e8d33cfecb1fad60d591376777ed939e583ff36d452a35f9135f481baaf9`.
  This binary is uncommitted development evidence, not a claimed release SHA.
- Result confirms `listenerStopped=true`, `databaseRemoved=true`; owned database
  `oxsum_examples_86338500b0e5db8eff10e845`. Console output has counts only;
  private logs and source digests are retained outside Git under
  `local-notes/operator-launch/example-isolation-run-three/`.
- The default build/pack path also passes all 52 essential assertions with a
  fresh server build and freshly packed SDK; no `--binary`/`--sdk-tarball` was
  supplied. The maintained example dependency remains selected explicitly.
  Binary SHA-256 `f64a3dddb89b89387a5b684d27207ae33e8309d176c0bc5bb5ed8a682ab19a25`,
  SDK SHA-256 `8261ad2f7462f9f4d58b11ae3323c0a2c5d34a16c992222b285e52aab297dfa4`.
  The result confirms removal of owned database `oxsum_examples_f65007428fd4d8e9e19e1051`
  and listener shutdown. Evidence: `local-notes/operator-launch/example-isolation-default/`.
- Formatter/diff checks, strict workspace Clippy and actionlint over both touched workflows pass.

The SDK fixture verifier runs after the server and database have stopped. It
rejects changed evidence/hash/key/head. Consumption's newly closed-day export
is empty; the immediate-settlement demo in PR #751 covers that separate claim.
No paid upstream or external mail/payment attempt occurs. CI and real reviewer
approval, PR #751 integration and final candidate
rerun remain distinct gates.
