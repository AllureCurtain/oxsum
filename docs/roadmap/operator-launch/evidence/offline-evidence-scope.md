# Offline evidence scope (#876)

Date: 2026-11-09. Parent: N12 / #738. This is a consumer repair, not final
candidate, browser, real-provider or publication acceptance.

## Defect and correction

The evidence SDK wrapper derives the ledger origin from the supplied note.
It can validate a genuinely signed foreign-origin note without binding the
customer's separately retained expected origin. The consumer now explicitly
checks that origin/key, bundle/customer identity and optional manifest digest.
Bill identity/head and consistency's ending head are also bound to the retained
inputs. Unsigned evidence cannot satisfy this signed-key exercise.

`verifyBill().ok` is an inclusion/arithmetic verdict: missing or invalid
price/sale bindings remain explanatory text. The consumer separately requires
`verifyPriceHead` / `verifySaleHead`, the retained catalog signing key and any
retained catalog version. A sale-priced bill requires its sale proof; an absent
wholesale proof is permitted. A supplied but invalid wholesale proof fails.
The core SDK verdict semantics are unchanged.

Positive verification uses original JSON strings. Lossless field selection and
mutation retain integer tokens above 2^53 and reject duplicate fields. Every
negative mutation changes the selected bytes; empty artifact scopes explicitly
skip unavailable coverage. JavaScript programming errors fail the program
instead of being counted as expected cryptographic rejections.

## Executed checks

The clean external project `/tmp/oxsum-876-consumer` installed the retained
`verify-oxsum-bill@0.1.0` tarball (SHA-256
`ecc6e8d33cfecb1fad60d591376777ed939e583ff36d452a35f9135f481baaf9`).
It imports only the installed package and local example modules/fixtures.

- Seven scope regression groups passed, including actual packed WASM for the
  v13 wholesale and v14 sale bills, valid foreign-origin signature refusal,
  wrong key/customer/identity, missing or altered catalog proof, and consistency
  ending at a different head. The underlying SDK's permissive verdict is
  asserted before the stricter consumer refusal, reproducing the old gap.
- The v13 bill plus v2 evidence consumer passed: altered amount/rate, actual
  committed artifact, missing manifest/artifact, wrong hash/key/origin/customer
  and archived head are rejected. No-op mutations are refused by construction.
- Both whole-response and SSE export directories from the #869 isolated local
  upstream fixture passed the consumer. Their actual development binary is
  `8d18170f85c9648301d63659b1ae28763eaf617c` (schema 0111); neither used a real
  provider. `fetch` was disabled and never called during verification.
- The large-integer adapter check observes unchanged SDK input bytes; it does
  not assert a genuine cryptographic proof over a synthetic large-value entry.
- Formatting, shell syntax, Python caller syntax and diff checks passed. Caller
  scripts now copy the required helper alongside `verify.mjs`; the candidate
  clean-pack script also runs the seven scope groups against its installed SDK.

Logs: `/tmp/oxsum-876-{scope,consumer,whole,sse}.log`. Missing witnesses remain an
explicit skip. The consumer's fetch guard is not an OS-level network sandbox.
Independent source review and PR integration are recorded in the PR. Re-run the
candidate's actual exported evidence and original SDK bytes after software
completion; no final N12 acceptance follows from these development fixtures.
