# Verifying an oxsum bill

You were handed a link like `https://…/bill/oxb-…`. That page is your bill, and you can check it
yourself: the verification runs in your browser, not on the operator's server.

This guide is for the person holding the link. It does not assume you have an account, a key, or
access to the repository.

## What the page shows

- The customer the bill is for, and the period it covers.
- The number of settled requests and the total charge.
- One row per request: its date, model, outcome in plain words, tokens in/out, and credits.
- The operator's public key, and the ledger head the rows belong to (its size and root hash).

Those figures come from a report the organization froze when it issued the bill. Re-issuing the
same period replays the same commitment — a bill cannot grow after the fact to absorb usage the
organization left out.

## Run the check

Press **Verify this bill**. Your browser downloads the verifier — the same Rust code the server
uses, compiled to WebAssembly — runs it over the bundle embedded in the page, and prints a verdict
plus a prefix of the verifier's SHA-256.

The check passes when all of the following hold:

1. Every shown row's bytes hash to the content hash recorded for it (nothing was edited).
2. The shown rows fold to the subset digest the report committed.
3. The report's marker entry names exactly that digest, and the marker's description carries the
   count and total shown on the page.
4. The marker and every shown row are in the ledger the head covers (inclusion proofs).
5. The head is signed by the operator key the bundle names — when the deployment signs heads.
6. If the deployment pins witnesses, the notes stored for this head verify as cosignatures under
   the pinned keys. The page says "witnessed by N", "witnesses are configured, but no cosignature
   covers this head yet", or "no witness keys are pinned" — three different facts.

If any of those fail, the verdict names the step that failed. Do not pay a bill whose check fails;
send the page URL and the verifier hash to the operator.

## What it proves — and what it does not

The check proves that **these rows are in this ledger, and the figures shown are the ones the
organization committed to**. It does not prove:

- **Completeness.** Without the anchored proof below, the entry count and total are the
  organization's signed claim, not a fact the bundle can demonstrate: if you kept your own request
  log, compare the count and the total against what you sent. When the report carries a
  `completeness` proof — the deployment anchors each closed day's customer log into the ledger —
  the bundle lists every day leaf of your log and the check rebuilds each day's leaf from the rows
  it assigns to that day. A dropped day leaves a gap in the anchored head; a row moved to another
  day changes the leaf. What even the anchored proof cannot show: that the organization counted
  the tokens the model really returned (the next bullet).
- **Charge arithmetic.** The check binds each entry's bytes to its hash; it does not recompute each
  row's price × usage. The operator's `/verify` page runs that per-bill check when you paste a
  single settlement's bundle.
- **Write-time honesty.** An entry proof binds bytes to a retained hash and head; it cannot prove the
  writer counted the tokens the model really returned. Your own copy of the upstream response
  is the counter-check.
- **That anyone vouched for the head.** A passing cosignature means the holder of the pinned witness key signed the
  head. Independence is a separate operational fact; it does not mean the witness
  audited every row, and witnesses can collude. The page says
  which case you are in rather than implying all three are the same.

## Trust anchors

- **The operator key.** The page prints the public key the bundle carries. The deployment publishes
  the same key at `GET /api/v1/log/key`; compare the fingerprints through a channel you trust
  (the operator's repository, a signed release note), not through the same page.
- **The verifier hash.** The verdict shows a prefix of the verifier artifact's SHA-256. Quote it
  when you dispute a result; compare it against an artifact the operator publishes if one exists.
- **The head.** The page prints the ledger head's size and root. If you archived an earlier head,
  ask the operator for a consistency proof that the current head extends it.

## Retain inputs from the customer portal

The **Bills → Retained ledger trust** card verifies and saves a signed head,
operator key and expected origin for the current customer. Later checks execute
WASM against that original root and key; they do not replace the archive. The
first key still comes from the server, so compare it independently. Missing WASM,
a changed key, rollback or a failed proof is never presented as a successful
continuity check. An empty archived log is labeled as signature-only evidence.

**Download retained inputs** produces `oxsum-trust-<organization>.json`. Its
`retained` object has `origin`, `key`, `head` and `note`; `checked`, when present,
contains the current signed head and a `consistency` response for a grown log.
Save that file outside browser storage. With a separately retained SDK, pass
`retained.note`, `JSON.stringify(retained.head)`, `retained.origin` and
`JSON.stringify(retained.key)` to `verifySignedHead`; pass the downloaded
`checked.consistency` with the same retained head/origin/key to
`verifyConsistency`. Never replace those inputs with ones freshly served by the
operator to resolve a failed check. The export does not include entry proofs or
period bundles; save those separately for the checks below.

## Verify offline, without the server

The bill page runs the installed verifier in your browser. If you want a check that outlives the link — or you want to hold the
evidence yourself — the same verifier is packaged as `verify-oxsum-bill`, and
it needs no account and no connection to the billing server after its dependencies and
evidence are saved.

For an individual entry, save the following while access works. A usage report,
statement and period evidence bundle have different formats and verifier calls;
do not pass a report bundle to `verifyBill`.

- the individual entry proof (`GET /api/v1/entries/{entryId}/proof`);
- the 64-hex content hash the bill printed — your record, not the bundle's own
  `contentHash`;
- if you want the signature check, the operator key from
  `GET /api/v1/log/key` or `GET /api/v1/log/head`, obtained through a channel
  you trust.

Then, in a separate project:

```bash
npm install /path/to/verify-oxsum-bill-0.1.0.tgz

node --input-type=module -e '
  import { verifyBill } from "verify-oxsum-bill";
  import { readFileSync } from "node:fs";
  const bundle = readFileSync("bundle.json", "utf8");
  const hash = readFileSync("content-hash.txt", "utf8").trim();
  console.log(await verifyBill(bundle, hash));
'
```

A passing entry verdict establishes hash/inclusion and reports the charge,
price and sale checks separately. Read every returned field: a non-settlement or
an old/new schema can skip arithmetic, and `ok: true` can coexist with an absent
or invalid price/sale binding sentence. This function does not authenticate the
ledger head. Use `verifySignedHead` with the expected origin and independently
trusted operator key, then `verifyConsistency` against a previously archived
head. A fresh key bundled with fresh evidence cannot prove continuity with the
key you originally trusted.

The `/verify` form has a server fallback when the WASM assets are unavailable;
that fallback is not an independent browser execution. A separately installed,
retained SDK and its artifact checksum let you verify offline without relying
on newly served code. Neither path proves honest upstream token counts.

The repository's [`examples/verify-consumer`](https://github.com/AllureCurtain/oxsum/tree/main/examples/verify-consumer)
shows the full form: settlement bundle, signed head, consistency against an
archived head, and the rejection cases (tampered content, a different hash, a
different operator key, a different archived head). The package was not on npm
as of 2026-11-04; the example validates the locally packed tarball, and the
publication step is separate.

## Verifying a whole billing period

An organization can also hand you a **version-2 evidence bundle** — one file
covering an entire closed period (usage records, statements, invoices, payment
orders, refunds, disputes, credit notes and the day's seal digests), rather than
a single settlement or a usage report. You reach it through an
`/api/v1/evidence/{token}` link the organization minted for you, or your own
account's `GET /api/v1/org/evidence-bundles` export.

The verifier function is `verifyEvidenceBundle` from the same
`verify-oxsum-bill` package:

```bash
node --input-type=module -e '
  import { verifyEvidenceBundle } from "verify-oxsum-bill";
  import { readFileSync } from "node:fs";
  const bundle = readFileSync("evidence-bundle.json", "utf8");
  const keyJson = readFileSync("trusted-operator-key.json", "utf8");
  console.log(await verifyEvidenceBundle(bundle, "<your organization id>", keyJson));
'
```

The check passes when every artifact the bundle's manifest names re-digests to
the figure the ledger committed, the manifest itself re-digests to the digest
the ledger's marker entry bound, the money rows' ledger entries sit inside the
supplied tree head, and a present note verifies under the selected operator key.
An unsigned bundle can pass its digest/inclusion checks without a signature;
inspect the note and retain that weaker status. Supplying a key does not turn a
missing note into an attestation. A dropped row, a changed amount or a bundle minted for a different organization
each fail the check by name.

The bundle's `scope` block is the honest half: it names what the bundle does
*not* cover — the platform's own supplier costs (redacted unless the operator
opts in), another organization's rows, history outside the committed period,
and anything still in flight when it was minted. It proves the committed set within that scope. It does not recompute every
settlement charge, establish the unprovided supplier books, or prove that the
operator recorded every token the model returned.

## If the link stops working

A bill link expires — 30 days by default, 90 at most — and the operator can
revoke one. An expired, revoked or unknown link answers the same `404`; ask the
operator for a new link. A link is a capability: anyone holding it can read
that bill, so treat it like a password and do not forward it more widely than
you would the bill itself.

An evidence-bundle link (`/api/v1/evidence/{token}`) works the same way but
answers `410` when it is dead — expired, revoked or out of downloads — and each
download is counted, so the operator can see whether the token travelled
further than you did.

## For operators

- The link is the credential. It is not indexed, not cached by shared caches, and carries no
  referrer; but it is a bearer capability, so issue one per recipient and revoke the ones you no
  longer want.
- To reissue, mint another link from **Reports** (`/app/reports`); to stop one, revoke it there. The
  page lists the links with their expiry and last use.
- What to tell a customer: "Open the link, press Verify this bill, and compare the key fingerprint
  with the one we publish. The check proves the rows are in our ledger; it does not prove we
  captured every row — your own request log is the cross-check."
