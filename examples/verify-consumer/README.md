# Independent verification consumer

A deliberately small consumer of the `verify-oxsum-bill` npm package. It proves
that a customer can check exported evidence with the packaged verifier, in a
project that has no workspace imports and no connection to the billing server.

Run the whole check from the repository root:

```bash
scripts/check-verify-consumer.sh
```

The script builds the wasm module, runs `npm pack` in `web/verify-sdk`, checks
the tarball's contents, installs the tarball into a fresh temporary project,
copies this directory's fixtures in, and runs `node verify.mjs` and the real-WASM
scope regression tests. No registry
publication is involved.

To run the example against an already-installed package, from this directory:

```bash
npm install /path/to/verify-oxsum-bill-0.1.0.tgz
node verify.mjs --dir /path/to/retained-export
node --test scope.test.mjs
```

`node verify.mjs --hash <64-hex>` checks the same evidence against a different
customer-held hash — the rejection check, when the hash is not the one the
entry records.

## The fixture

One genuine settlement from an isolated ledger created by the server's own
wallet API:

| File | What it is |
| --- | --- |
| `fixtures/manifest.json` | The customer's record: entry id, content hash, ledger origin, head size/root, price version |
| `fixtures/settlement-bundle.json` | `GET /api/v1/entries/{entryId}/proof`, `data` |
| `fixtures/signed-head.json` | `GET /api/v1/log/head`, `data` |
| `fixtures/operator-key.json` | `GET /api/v1/log/key`, `data` |
| `fixtures/held-head.json` | The head the customer archived before the turn (`consistency.oldHead`) |
| `fixtures/consistency.json` | `GET /api/v1/log/consistency?from=1`, `data` |
| `fixtures/checkpoint.json` | `GET /api/v1/log/checkpoint` — `data: null`, because this deployment pins no witnesses |

The settlement is a 116-input / 100-output-token turn on channel `mock`, model
`mock-a`, price version 1, charged 316 minor units (0.000316 credits). The
capture used a test head-signing seed, so the signatures are genuine over this
fixture but do not belong to any live deployment. Do not treat the key in
`operator-key.json` as an operator's production key.

## What the checks prove

- **Bill** — `verifyBill(bundle, expectedHash)` recomputes the settlement
  entry's content hash from its canonical bytes, links that hash to the tree
  head with the inclusion proof, recomputes the charge from the usage and rates
  inside the entry. Its `ok` alone does not require successful price/sale
  checks. This consumer separately calls `verifyPriceHead` / `verifySaleHead`
  and requires every supplied catalog binding to use the retained operator key.
  Wholesale-priced bills require the price binding; sale-priced bills require
  the sale binding and may omit the wholesale binding. Retained price or sale
  versions, entry identity and head size/root are also compared. This example
  targets usage settlements, not arbitrary non-settlement ledger entries.
- **Head** — `verifySignedHead(note, head, origin, key)` verifies the
  operator's Ed25519 signature over the C2SP note, that the note names the
  expected ledger origin, and that it attests the head being checked. The key
  must be retained through a channel you independently trust; fetching it
  together with fresh evidence does not establish historical continuity. The
  copy in this fixture is only a captured sample.
- **Consistency** — `verifyConsistency(response, heldHead, origin, key)` checks
  that the response's old head equals the head the customer archived, that the
  proof links it to the new head, and that the new head is signed. This is the
  check that answers "the log only grew; it did not rewrite what I saw". The
  response must end at the same head as the bill under review.
- **Witnesses** — `verifyWitnesses(notes, pins, origin, head)` checks each
  cosignature against a pinset the customer chose. This fixture has no
  cosignatures, so the example says so and skips the check rather than claim a
  witness vouched.

Original JSON strings go directly to the SDK. Field selection and test mutations
preserve numeric tokens above JavaScript's safe-integer range; duplicate fields
are refused. Parsed objects are used only for display and nonnumeric identity
selection, never to reconstruct positive proof input. Version-2 evidence must
match the retained bundle/customer identity and explicit ledger origin, even if
its own note carries a valid signature for another origin. A retained manifest
digest is also checked when supplied. This signed-key exercise refuses unsigned
evidence; it does not claim that unsigned digest/inclusion checks are impossible.

The rejection checks show that altered amounts/rates/catalog rows, missing
required catalog bindings or committed artifacts, a different customer-held
hash, a different operator key/origin/customer, and a different archived head
each fail. An empty artifact scope is reported as unavailable tamper coverage.
The program disables `fetch`; the verifier only reads local package/WASM bytes.
This is not an operating-system network sandbox. The
verifier does not fall back to trusting the bundle's own `contentHash` or the
server's current answer.

## What it does not prove

- **Completeness.** Unless the evidence carries the anchored-day proof, the
  entry count and total are the organization's commitment, not a fact this
  bundle can demonstrate. Compare them with your own request log.
- **Write-time honesty.** A ledger can prove it was not rewritten; it cannot
  prove the writer counted the tokens the model really returned.
- **That anyone vouched for the head.** A passing cosignature means a witness
  saw the head; it does not mean the witness audited every row, and witnesses
  can collude. No cosignature is not the same as a failed one.
- **Price correctness beyond the binding.** The signed price checkpoint binds
  the row the entry names; it does not prove the operator's price was fair.

Inclusion, arithmetic, signature, consistency, and completeness are separate
claims, and the output names which ones actually ran.

## Tested version

`verify-oxsum-bill@0.1.0`, packed locally. The package is not published to npm
yet (a registry query returns E404); publication is a separate release step
that needs the maintainer's credentials. `scripts/check-verify-consumer.sh`
prints the exact tarball it tested.

## Reproducing the fixture

The fixture was written by a temporary integration test over the server's own
in-process app and wallet with a fixed head-signing seed, then removed from the
tree once the JSON was captured. The same flow is covered by
`crates/server/tests/heads.rs` (signed heads, consistency, price checkpoints)
and `crates/server/tests/bills.rs` (proof bundles). The example is the
consumer-side check, not a second fixture generator.


## Sale fixture and scope regression provenance

`fixtures/sale/` contains the bill, retained manifest/head and public operator key
from the isolated whole-response fixture of #869 (development binary
`8d18170f85c9648301d63659b1ae28763eaf617c`, schema 0111). The local upstream returned
23 input / 11 output tokens; the v14 sale-priced bill charged 34 minor units.
No real provider was called, and no key is a private or production credential.
The four files exercise separate required-sale versus optional-wholesale checks
through the packed WASM. They are not final candidate or public SDK evidence.

`scope.test.mjs` also signs a foreign-origin note with a generated test key and
shows that the underlying evidence wrapper accepts its self-declared origin,
while this consumer rejects it against the separately retained expected origin.
The large-integer transport test uses a recording adapter; it proves unchanged
bytes, not a valid signature over an invented high-value ledger entry.
