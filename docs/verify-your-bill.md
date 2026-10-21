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

If any of those fail, the verdict names the step that failed. Do not pay a bill whose check fails;
send the page URL and the verifier hash to the operator.

## What it proves — and what it does not

The check proves that **these rows are in this ledger, and the figures shown are the ones the
organization committed to**. It does not prove:

- **Completeness.** The entry count and total are the organization's signed claim, not a fact the
  bundle can demonstrate. If you kept your own request log, compare the count and the total against
  what you sent.
- **Charge arithmetic.** The check binds each entry's bytes to its hash; it does not recompute each
  row's price × usage. The operator's `/verify` page runs that per-bill check when you paste a
  single settlement's bundle.
- **Write-time honesty.** A ledger can only prove it was not rewritten; it cannot prove the person
  writing it counted the tokens the model really returned. Your own copy of the upstream response
  is the counter-check.

## Trust anchors

- **The operator key.** The page prints the public key the bundle carries. The deployment publishes
  the same key at `GET /api/v1/log/key`; compare the fingerprints through a channel you trust
  (the operator's repository, a signed release note), not through the same page.
- **The verifier hash.** The verdict shows a prefix of the verifier artifact's SHA-256. Quote it
  when you dispute a result; compare it against an artifact the operator publishes if one exists.
- **The head.** The page prints the ledger head's size and root. If you archived an earlier head,
  ask the operator for a consistency proof that the current head extends it.

## If the link stops working

A link expires — 30 days by default, 90 at most — and the operator can revoke one. An expired,
revoked or unknown link answers the same `404`; ask the operator for a new link. A link is a
capability: anyone holding it can read that bill, so treat it like a password and do not forward it
more widely than you would the bill itself.

## For operators

- The link is the credential. It is not indexed, not cached by shared caches, and carries no
  referrer; but it is a bearer capability, so issue one per recipient and revoke the ones you no
  longer want.
- To reissue, mint another link from **Reports** (`/app/reports`); to stop one, revoke it there. The
  page lists the links with their expiry and last use.
- What to tell a customer: "Open the link, press Verify this bill, and compare the key fingerprint
  with the one we publish. The check proves the rows are in our ledger; it does not prove we
  captured every row — your own request log is the cross-check."
