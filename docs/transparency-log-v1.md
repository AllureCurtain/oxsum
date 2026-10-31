# OXSUM Billing Transparency Log v1

The wire protocol a bill verifier speaks. One document naming every object the ledger signs and every check that proves it — the normative reference for `oxsum-verify`, the `/verify` page, the `verify-oxsum-bill` npm package, and any third party that wants to verify a bill without trusting the server.

Version: **v1**. Any change to entry encoding, `SCALE`, hash domains, signed-note format, or the description schemas revs this document. The product's prose companion is [docs/proofs.md](proofs.md) — that page says what each check *establishes*; this page says *how* it is computed.

## 1. What a verifier holds

A verifier needs only three things, all public:

- **The proof bundle** — `GET /api/v1/entries/{entryId}/proof` returns the entry, an inclusion proof, the tree head as of the proof, and optionally a price link.
- **The content hash** — the 64-hex BLAKE3 hash printed on the bill. The verifier recomputes it from the entry's canonical bytes; the hash the server returns this time does not count.
- **The published key** — `GET /api/v1/log/key` answers the operator's Ed25519 verifying key. The verifier obtains it through a channel it trusts (a pinned value, an out-of-band document, a witness's own pin).

No authentication, no session, no endpoint beyond the two GETs above.

## 2. Terminology

| Term | What it is |
| --- | --- |
| `Entry` | A balanced ledger entry — the canonical bytes the content hash commits to |
| `TreeHead` | `{size, root}` — the Merkle accumulator's state at a point in the log |
| `InclusionProof` | The sibling hashes that link an entry's content hash to a head's root |
| `ConsistencyProof` | The hashes that prove one head extends an earlier, smaller head |
| `SignedHead` / `note` | A C2SP `tlog-checkpoint`: the head's origin, size, root, plus signature line(s) |
| `WitnessPin` | `{witnessUrl, keyName, publicKey, keyHash}` — a third-party cosigner's pinned key |
| `WitnessNote` | `{witnessUrl, note}` — a stored cosignature line on the checkpoint |
| `PriceLink` | The signed binding between a settlement's `(channel, model, priceVersion)` and the catalog row it was charged under |
| `SCALE` | `6` — one credit is `1_000_000` minor units, the precision the entry encoding depends on |
| `keyHash` | The 4-byte selector naming which signature line on a note a key owns — `SHA-256(keyName ‖ '\n' ‖ 0x04 ‖ publicKey)[..4]`, hex |

## 3. Hash domains

Every hash is domain-separated — `BLAKE3(tag.len() as u64 little-endian ‖ tag ‖ payload)` — so a digest computed under one purpose can never collide with another. The tag length is length-prefixed so `tag ‖ payload` parses unambiguously. The domains the protocol uses:

| Domain | Payload hashed |
| --- | --- |
| `oxsum/price-catalog/v1` | The canonical price row the PriceLink's note attests |
| `oxsum/statement-window/v1` | The digest a statement anchor's window field folds to |
| `oxsum/statement-rollup/v1` | The digest a statement's canonical rollup folds to |
| `oxsum/usage-report/v1` | The marker's commitment (`{v, endUser, from, to, entryCount, chargedTotalMinor, subsetHash}`) |
| `oxsum/usage-day/v1` | One day leaf in a customer's day log |
| `oxsum/usage-anchor-map/v1` | The sorted-map root over every customer's day-log head at the anchor day |
| `oxsum/idempotency/v1` | The idempotency-record body |
| `oxsum/key-spend-limit/v1` | A key's committed-spend predicate input |
| `oxsum/credit-limit/v1` | A credit-line grant predicate input |
| `doubleentry/…` | Reserved for the engine's own domains — never used by application code |

## 4. Wire shapes

All objects are JSON. Field names are camelCase on the wire; `optional` fields are absent (never `null`) unless noted.

### 4.1 ProofBundle

```json
{
  "entry": { "…": "the balanced entry, canonical bytes hashed to contentHash" },
  "head": { "size": 132, "root": "84433c54476f…" },
  "proof": { "…": "sibling hashes that link entry.contentHash to head.root" },
  "price": {
    "channel": "deepseek",
    "model": "deepseek-chat",
    "version": 1,
    "row": "the canonical JSON row the note attests",
    "note": "the C2SP signed-note text",
    "keyName": "oxsum/tree-heads",
    "publicKey": "2870259b…",
    "keyHash": "2870259b"
  }
}
```

`price` is present only on settlements — holds, top-ups and adjustments carry none — and only on a deployment with `OXSUM_HEAD_SIGNING_KEY` configured.

### 4.2 SignedHead

```json
{
  "note": "oxsum/ledgers/<tenant>\n<size>\n<root base64>\n\n— <keyName> <signature base64>\n",
  "origin": "oxsum/ledgers/<tenant>",
  "keyName": "oxsum/tree-heads",
  "publicKey": "…32 bytes hex…",
  "keyHash": "…4 bytes hex…",
  "size": 132,
  "root": "…32 bytes hex…"
}
```

The `note` is the C2SP `tlog-checkpoint` wire: origin, size, root base64, blank line, then `— name signature` per signer. The verifier parses it and checks the signature against `publicKey`/`keyHash`.

### 4.3 ConsistencyProof

The proof object `GET /api/v1/log/consistency?from=` returns. Its internal layout follows doubleentry's `ConsistencyProof` — a list of subtree roots the verifier folds through the same algorithm the inclusion proof uses.

### 4.4 WitnessPin / WitnessNote

```json
{
  "witnessUrl": "https://witness.example.com",
  "keyName": "witness/key",
  "publicKey": "…32 bytes hex…",
  "keyHash": "…4 bytes hex…"
}
```

```json
{
  "witnessUrl": "https://witness.example.com",
  "note": "the cosigned C2SP note — same shape as SignedHead plus the cosignature line"
}
```

### 4.5 StatementAnchorCheck

The statement's `anchor` field plus the `window`/`rollup` inputs the check folds. Wire fields are the statement's own figures re-serialized to the canonical `StatementRollupV1` and `StatementRollupLine` shapes the core anchors.

### 4.6 UsageReportBundle

```json
{
  "reportId": "…",
  "endUser": "…",
  "from": "2026-10-01",
  "to": "2026-10-31",
  "entryCount": 42,
  "chargedTotalMinor": 1234567,
  "subsetHash": "…32 bytes hex…",
  "marker": { "entry": {…}, "contentHash": "…", "proof": {…} },
  "entries": [ { "entry": {…}, "contentHash": "…", "proof": {…} }, … ],
  "head": { "size": N, "root": "…" },
  "note": "…optional signed-note…",
  "keyName": "…",
  "publicKey": "…",
  "keyHash": "…",
  "completeness": {
    "v": 1,
    "anchor": { "entry": {…}, "day": "YYYY-MM-DD", "mapRoot": "…", "enduserCount": N },
    "head": { "size": N, "root": "…" },
    "mapProof": {…},
    "days": [ { "day": "YYYY-MM-DD", "index": N, "leaf": "…", "proof": {…}, "rows": [i,…] }, … ]
  }
}
```

`completeness` is present only when the span is covered by a daily anchor — the day log that seals each customer's day in the ledger.

## 5. The checks

Each check is a pure function: inputs → verdict, no I/O, no clock, no randomness.

### 5.1 `verify_bundle` — entry inclusion

```
1. Parse the bundle: entry, head, proof.
2. Recompute the content hash from the entry's canonical bytes
   (Entry::adopt_verified — a single flipped byte fails it).
   The recomputed hash must equal the expected_hash the verifier holds.
3. Verify the inclusion proof: the recomputed hash + proof siblings
   fold to head.root.
```

Verdict: `true` or `false`. The check answers "this entry is in this ledger and hashes to what was recorded."

### 5.2 `verify_bill` — the composed verdict

`verify_bill` runs `verify_bundle` first; on `false` the verdict is `"fail"` and both sentences are empty. When the entry includes, it adds:

- `verify_charge` — recomputes the recorded charge from the settlement description's `usage` and `lines` (schema `v` 2 through 8; `charged`/`freeze` are checked against the line fold). A description the verifier does not parse as a settlement reports `NotASettlement`; a `v` below the known floor reports `OlderSchema`; one above reports `NewerSchema{version}` — each with the reason named in the sentence rather than a bare pass/fail.
- `verify_price_head` — checks the PriceLink (see §5.6).

Verdict: `"pass"`, `"chargeMismatch"`, or `"fail"`. `charge` and `price` are whole sentences (each carries its leading space so a renderer concatenates after the inclusion clause); a `v7+` record's `mandate` line is folded into the charge sentence.

### 5.3 `verify_signed_head` — the operator's attestation

```
1. Parse the note (a SignedTreeHead): origin, head, signature lines.
2. Reject if origin ≠ expected_origin — a signature for another tenant's
   log answers a different question.
3. Reject if the note's attested head ≠ the served head.
4. Find the signature line under (keyName, keyHash).
5. Verify the Ed25519 signature over the note body against publicKey.
```

Verdict: `Ok` or one of `Malformed`, `WrongOrigin`, `HeadMismatch`, `Unsigned`, `BadSignature`.

### 5.4 `verify_consistency` — extension of a held head

```
1. Reject if old_head ≠ held — a proof anchored elsewhere proves nothing.
2. Verify the consistency proof: old_head → new_head extends by append-only.
3. Run verify_signed_head on the new head's note.
```

Verdict: `Ok` or `OldHeadMismatch`/`Inconsistent`/the signed-head failures.

### 5.5 `verify_witness` — a third-party cosignature

```
1. Parse the witness note.
2. Reject if origin ≠ expected_origin or head ≠ expected_head.
3. Find the WitnessPin matching witness_url.
4. Find the cosignature/v1 line under (pin.keyName, pin.keyHash).
5. Verify it against pin.publicKey — returning the timestamp inside the
   signed message.
```

Verdict: `Verified{at}` or `Malformed`/`WrongOrigin`/`HeadMismatch`/`UnknownKey`/`BadSignature`.

### 5.6 `verify_price_head` — the price binding

```
1. Parse the bundle's price field.
2. Reject if its channel/model/version ≠ the settlement description's.
3. Parse the note; reject if origin ≠ "oxsum/price-catalog/{channel}".
4. Reject if note.head.size ≠ link.version.
5. Recompute Hash::digest(PRICE_ROW_DOMAIN, link.row); reject if ≠ note.head.root.
6. Verify the signature under (keyName, keyHash, publicKey).
```

Verdict: `Ok` or `Malformed`/`WrongOrigin`/`BindingMismatch`/`HashMismatch`/`Unsigned`/`BadSignature`/`Absent`.

### 5.7 `verify_statement_anchor` — the statement's log anchor

```
1. Parse the statement and its anchor.
2. Reject if the note's origin ≠ "oxsum/ledgers/{tenant}".
3. Reject if the note attests a head other than the one right after the anchor.
4. Fold the covered entries' content hashes under STATEMENT_WINDOW_DOMAIN;
   reject if ≠ anchor.windowHash.
5. Re-serialize the statement's canonical rollup; reject if its digest under
   STATEMENT_ROLLUP_DOMAIN ≠ anchor.rollupHash.
6. Verify the signature.
```

Verdict: `Ok` or `Malformed`/`WrongOrigin`/`HeadMismatch`/`WindowMismatch`/`RollupMismatch`/`Unsigned`/`BadSignature`/`Absent`.

### 5.8 `verify_usage_report` — a signed end-user report

Before any check runs, every entry in the bundle — the marker and each shown row — is `adopt_verified` against its recorded `contentHash`: a tampered entry fails adoption with `Malformed` before its proof is ever asked. The signature inputs are the bundle JSON, `expected_origin`, `expected_end_user`, and an optional `key` the caller fetched through a channel it trusts.

```
1. Descriptor self-consistency: digest USAGE_REPORT_DOMAIN over the shown
   entries' recomputed content hashes (concatenated in order); it must equal
   the descriptor's subsetHash (DescriptorMismatch).
2. Marker binding: the marker's DocumentRef carries id "oxsum/usage-report/v1"
   whose content hash equals that digest (MarkerMismatch).
3. Commitment match: the marker's description re-parses as the canonical
   commitment — v==1 and {endUser, from, to, entryCount, chargedTotalMinor,
   subsetHash} all equal the descriptor's (CommitmentMismatch).
4. Inclusion: the marker and every shown entry's proof verifies against head
   (Inclusion).
5. Subset shape: entryCount == shown.length (SubsetMismatch) and
   end_user == expected_end_user (a wrong-holder report is Malformed naming
   the mismatch).
6. Signed head: when the bundle carries a note, the served keyName/publicKey/
   keyHash — if present — must agree with the held key, then the note verifies
   under expected_origin. No note means the deployment signs nothing: the
   report is verified to the digests alone, an Ok.
```

Verdict: `Ok` or `Malformed`/`MarkerMismatch`/`CommitmentMismatch`/`Inclusion`/`SubsetMismatch`/`DescriptorMismatch`/`Unsigned`/`BadSignature`/`Completeness`.

When `completeness` is present it runs after step 5, before the signed head — its failures are all `Completeness(<reason>)` with the message naming which:

```
a. The anchor entry adopts; its DocumentRef carries "oxsum/usage-anchor/v1"
   bound to anchor.mapRoot, and its description's {v, day, mapRoot,
   enduserCount} equals the proof's.
b. The anchor's own inclusion proof verifies against the bundle head — the
   anchor is a fact of this ledger.
c. anchor.day is not before the report's `to` — the anchor covers the span's
   last days.
d. Every day leaf's proof verifies into completeness.head at its index, and
   the indices are exactly 0..head.size with no gap and no repeat — the day
   log is complete by construction.
e. For every shown row: it is claimed by exactly one day leaf, its
   booking_date is that leaf's day, and each claimed day's leaf rebuilds —
   MerkleAccumulator over the rows' content hashes, then
   usage_day_leaf(day, count, root) — to the attested leaf. A dropped,
   swapped or extracted row fails here.
f. The customer's head is the pair the anchor map fixes:
   usage_anchor_pair_leaf(endUser, head.root, size) verifies into mapHead
   {size: enduserCount, root: mapRoot} via mapProof.
```

## 6. Trust model

| Check establishes | Check does not establish |
| --- | --- |
| The entry hashes to the recorded content hash and sits in the ledger at this head | That the head is current, or that the ledger is complete |
| The operator's key signed this head | That the operator's key is the one the verifier should trust — that is a trust choice against a key obtained elsewhere |
| The new head extends an archived head | That nothing was withheld before the archived head — an append-only log can still omit, which is why completeness is stated, not proven |
| The settlement's charge recomputes from the entry's recorded usage and priced lines | That upstream really returned those tokens — the count is what the writer recorded, and the customer's own response log is the counter-check |
| The catalog row the bill charged under is the row the operator attested at that version | That the price was fair or that no other price ever existed |
| The statement's window and rollup hash to what the anchor committed | That a printed or exported PDF was not edited afterwards — the anchor covers the figures, not the rendering |
| The usage report's shown entries fold to the committed subset digest and each verifies inclusion | Per-row charge arithmetic (`verify_charge`), and completeness for an unanchored span — the anchored proof rebuilds day leaves and catches drops, the unanchored span keeps the older claim |
| The witness's cosignature verifies under the pinned key at a timestamp inside the signed message | That the witness audited every row, that witnesses do not collude, or that the pinset is the right set — a verifier may substitute its own |

## 7. Versioning

v1 is the wire shape the deployment serves today — SCALE `6`, the hash domains in §3, and the description schema versions the writers already emit: settlement `v` 2–8 recompute under `verify_charge` (v8 added `upstreamModel`, v7 `mandate`), and the commitment/anchor/rollup/usage-report documents are all `v == 1`. A change to any of these revs this document; readers implementing from it should name the version they verify against.
