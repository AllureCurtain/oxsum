# Current-authority isolated demo (#728)

Date: 2026-11-08. Base: `0314cbd3d5439adbc904a327a8e16704b344a0af`.
Scope: N02 / local J01. Development execution; no final candidate claim.

The README command built the server and WASM SDK, created a random database,
provisioned owner consent and an invited customer, replayed synthetic funding
without duplication, and published/assigned explicit sale terms. OpenAI Python
SDK 2.54.0 sent one SSE request to the local stub; psycopg 3.3.8 managed only
this invocation's database. Python 3.13.1, Node 24.22.1 and PostgreSQL 17 were
used. The first attempted run stopped on an uninstalled Python prerequisite
before any database was created; prerequisite checks now precede builds.

Observed request: `d73a7e60-cf58-40e8-ac84-628f6da37596`.
Settlement: `4a3ca583-5ad4-5ba1-ae45-36a7a0e77fbe`.
Retained content hash:
`c2818ae216629517d019801fcc91fe797b97ae4b378f7e533d27e08b20a1cff6`.

- Exactly 23 input + 11 output tokens at one minor unit/token: charge 34;
  available 99,999,966; reserved 0; exactly one stub attempt.
- The own database was dropped and listener stopped before a clean consumer
  installed the packed `verify-oxsum-bill` tarball outside the repository.
- Offline bill/hash/inclusion/charge/supply/sale, operator signature and
  consistency checks passed. Changed content, retained hash, public key and
  archived head were rejected. `fetch` was disabled and never called.
- Witness and closed-period bundle cases were explicitly skipped because this
  run tests immediate entry evidence. Neither is an essential step of this
  narrowly declared demo; neither closes an external or period acceptance gate.
- Shared consumption walkthrough: fresh-business and explicit-existing-owner
  runs both passed all 10 assertions. Its closed-day export has zero manifest
  rows and is labeled as such; it does not verify the turn made today.
- Formatting and strict workspace Clippy passed. Node syntax checks and
  diff whitespace checks passed. No production Rust/API field changed.
- Three isolation tests passed: denied CREATE cannot yield an app connection;
  exceptions drop only the own random database; inherited provider/payment/mail
  and database environment variables are excluded.

The local ignored `local-notes/operator-launch/demo-run-two/` directory retains
sanitized evidence, verifier tarball, checksums and verification output. No
external provider, PSP, mail, public registry or release endpoint was used.
The real-provider attempt/spend record remains zero.
