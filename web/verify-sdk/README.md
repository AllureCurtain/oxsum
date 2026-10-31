# verify-oxsum-bill

Verify an [oxsum](https://github.com/AllureCurtain/oxsum) bill entirely in the browser — recomputes the settlement's content hash from its canonical entry bytes, links it to the ledger's Merkle head, and checks the signed price binding, with no server round-trip.

This package wraps the `oxsum-verify` Rust crate compiled to WebAssembly. The same function body runs on the server and here in the user's own page, so "the server says it verified" is never the only evidence.

```ts
import { verifyBill } from "verify-oxsum-bill";

// `bundle` is the inner `data` of GET /api/v1/entries/{entryId}/proof,
// `hash` is the 64-hex content hash printed on the bill.
const verdict = await verifyBill(JSON.stringify(bundle), hash);
// verdict.ok — true when the entry verifies against the head
// verdict.detail — the human-readable reason, the same prose the page prints
// verdict.charge / verdict.price — the recomputed charge and price sentences
```

## API

| Function | What it checks |
| --- | --- |
| `verifyBill(bundleJson, hashHex)` | One settlement's proof bundle — hash recompute + inclusion + charge + price binding |
| `verifySignedHead(json)` | A ledger head's operator Ed25519 signature |
| `verifyConsistency(json, checkJson)` | A new head extends one the verifier already holds |
| `verifyWitnesses(json, pinsJson)` | Cosignatures a checkpoint carries, against a pinned witness set |
| `verifyStatementAnchor(json, checkJson)` | A statement's completeness anchor |
| `verifyUsageReport(json)` | A usage-report rollup's day/anchor leaves |
| `verifyPriceHead(json)` | A price-row checkpoint's signed binding |

Every answer is `{ ok, detail, … }` — `detail` is the human-readable sentence the server's own `/verify` page prints, so a page only has to render it.

The objects these functions speak — the proof bundle, signed-note head, `PriceLink`, the consistency and witness proofs — are written down field-by-field in [docs/transparency-log-v1.md](https://github.com/AllureCurtain/oxsum/blob/main/docs/transparency-log-v1.md), the protocol's normative reference.

## Embedding

The package ships the wasm-bindgen output inside `wasm/` — the loader instantiates it lazily on first call, so importing the module costs nothing until a verification actually runs. No configuration, no endpoint, no key.

## `<verify-bill>` — the check as an element

`verify-oxsum-bill/verify-bill` registers a `<verify-bill>` custom element that renders the whole check — the same verdict the `/verify` page prints — inside a shadow root on any page:

```html
<script type="module" src="verify-oxsum-bill/verify-bill"></script>

<!-- the element fetches the bundle itself -->
<verify-bill server="https://oxsum.example.com"
             entry="a8f3…uuid"
             expected-hash="e2b9…64hex"></verify-bill>

<!-- or render its own paste-a-bundle form -->
<verify-bill></verify-bill>
```

| Attribute | Meaning |
| --- | --- |
| `server` + `entry` | The element GETs `{server}/api/v1/entries/{entryId}/proof` itself |
| `bundle` | The bundle JSON verbatim — or left unset to render a paste form |
| `expected-hash` | The 64-hex content hash the *customer* recorded — the half the check cannot supply, never the bundle's own `contentHash` |

The wasm module instantiates on first verify, not on attach, so the element costs nothing until used; a browser without WebAssembly renders a readable fallback that points at the server's `/verify` page rather than failing blank.

The `server`+`entry` mode fetches the proof endpoint cross-origin — embed it on the same origin the endpoint serves, or a deployment that answers `Access-Control-Allow-Origin` for the page. A page that cannot reach the endpoint can still use `bundle` or the paste form, which needs no fetch at all.

## `<verify-statement>` — the statement check as an element

`verify-oxsum-bill/verify-statement` registers a `<verify-statement>` custom element that runs `verifyStatementAnchor` inside a shadow root — the statement counterpart to `<verify-bill>`, fed by the self-contained bundle `GET /api/v1/statements/{id}/proof` answers:

```html
<script type="module" src="verify-oxsum-bill/verify-statement"></script>

<!-- the element fetches the bundle itself -->
<verify-statement server="https://oxsum.example.com"
                  statement="a8f3…uuid"></verify-statement>

<!-- or render its own paste-a-bundle form -->
<verify-statement></verify-statement>
```

| Attribute | Meaning |
| --- | --- |
| `server` + `statement` | The element GETs `{server}/api/v1/statements/{id}/proof` itself |
| `bundle` | The proof JSON verbatim — or left unset to render a paste form |

The bundle is a superset of what the check consumes: the element splits it into the detail wire `{statement, lines, anchor}` and the check `{windowHashes, expectedHead, expectedOrigin}`, so a customer drops the endpoint's answer in and gets a verdict — no database read, no `/log/head` fetch, nothing to stitch. On a deployment that signs no heads the bundle's `anchor.note` and `expectedHead` are `null`, and the element says so: the window fold and the rollup digest are what was verified, not a signature that was never there.

The wasm module instantiates on first verify, not on attach, and a browser without WebAssembly renders a readable fallback rather than failing blank — the same rules `<verify-bill>` keeps.

## License

MIT OR Apache-2.0.
