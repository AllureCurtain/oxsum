#!/usr/bin/env node
/**
 * Verify exported oxsum evidence without the billing server.
 *
 * Run through `scripts/check-verify-consumer.sh`, which packs the npm tarball,
 * installs it into a clean temporary project, and invokes this script there.
 * The script itself only reads the fixtures beside it and the installed
 * `verify-oxsum-bill` package — no HTTP client, no account, no endpoint.
 *
 * The fixtures are one genuine settlement captured from an isolated ledger:
 *
 *   manifest.json            the customer's record: entry id, content hash,
 *                            ledger origin, head size/root, price version
 *   settlement-bundle.json   GET /api/v1/entries/{id}/proof `data`
 *   signed-head.json         GET /api/v1/log/head `data`
 *   operator-key.json        GET /api/v1/log/key `data`
 *   held-head.json           the head the customer archived before the turn
 *   consistency.json         GET /api/v1/log/consistency?from=1 `data`
 *   checkpoint.json          GET /api/v1/log/checkpoint (no witnesses here)
 *
 * A second, optional set carries the version-2 complete-period evidence
 * bundle (issue #616):
 *
 *   evidence-bundle.json      GET /api/v1/org/evidence-bundles/{id} `data`
 *   evidence-manifest.json    the bundle id, organization id and ledger origin
 *   evidence-operator-key.json the `{keyName, publicKey, keyHash}` the bundle serves
 *
 * The rejection checks mutate the inputs in memory; nothing is written.
 */

import { inspect, scopedBill, scopedConsistency, scopedEvidence } from "./scope.mjs";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  listBundleManifests,
  verifyBill,
  verifyConsistency,
  verifyEvidenceBundle,
  verifyPriceHead,
  verifySaleHead,
  verifySignedHead,
  verifyWitnesses,
} from "verify-oxsum-bill";

const here = dirname(fileURLToPath(import.meta.url));

function parseArgs(argv) {
  const args = { dir: join(here, "fixtures") };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (flag === "--dir") {
      args.dir = resolve(argv[++i]);
    } else if (flag === "--hash") {
      args.hash = argv[++i];
    } else {
      throw new Error(`unknown argument: ${flag}`);
    }
  }
  return args;
}

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

/** `{ok, detail}` for a check that returns a verdict, `{ok:false}` for one that throws. */
async function attempt(run) {
  try {
    return await run();
  } catch (error) {
    if (error instanceof ReferenceError || error instanceof TypeError) throw error;
    return { ok: false, detail: error instanceof Error ? error.message : String(error) };
  }
}

function report(label, ok, detail) {
  const status = ok ? "PASS" : "FAIL";
  console.log(`[${status}] ${label}`);
  if (detail) {
    console.log(`       ${detail}`);
  }
  return ok;
}

function expectRejected(label, verdict) {
  if (verdict.ok) {
    console.log(`[FAIL] ${label}: the verifier accepted the altered input`);
    return false;
  }
  const detail = verdict.detail ?? verdict.verdict ?? "rejected";
  console.log(`[PASS] ${label}: rejected — ${detail}`);
  return true;
}

async function main() {
  // Prove the claim in the output: the verifier runs from local bytes and the
  // installed package, so any attempt to reach the billing server is a bug.
  let networkAttempts = 0;
  globalThis.fetch = () => {
    networkAttempts += 1;
    throw new Error("the offline example must not fetch anything");
  };

  const args = parseArgs(process.argv.slice(2));
  const dir = args.dir;
  const manifestRaw = readFileSync(join(dir, "manifest.json"), "utf8");
  const bundleRaw = readFileSync(join(dir, "settlement-bundle.json"), "utf8");
  const manifest = JSON.parse(manifestRaw);
  const headRaw = readFileSync(join(dir, "signed-head.json"), "utf8");
  const head = JSON.parse(headRaw);
  const key = readJson(join(dir, "operator-key.json"));
  const held = readJson(join(dir, "held-head.json"));
  const checkpoint = readJson(join(dir, "checkpoint.json"));
  const expectedHash = args.hash ?? manifest.contentHash;

  // The v2 evidence-bundle fixture: a complete-period bundle minted for one
  // organization, the published operator key it carries, and the manifest
  // naming the organization and ledger origin the check binds. Optional — a
  // fixtures dir without it simply skips the v2 block.
  const evidencePath = join(dir, "evidence-bundle.json");
  const evidenceRaw = existsSync(evidencePath) ? readFileSync(evidencePath, "utf8") : null;
  const evidence = evidenceRaw ? JSON.parse(evidenceRaw) : null;
  const evidenceManifest = evidence
    ? readJson(join(dir, "evidence-manifest.json"))
    : null;
  const evidenceKey = evidence
    ? readJson(join(dir, "evidence-operator-key.json"))
    : null;

  const servedHead = inspect(bundleRaw).token(["head"]);
  const keyJson = JSON.stringify(key);
  let failures = 0;

  console.log("== Genuine exported evidence ==");
  console.log(`entry ${manifest.entryId}`);
  console.log(`content hash ${expectedHash}`);
  console.log(`ledger ${manifest.origin}`);
  console.log();

  const api = { verifyBill, verifyConsistency, verifyEvidenceBundle, verifyPriceHead, verifySaleHead, verifySignedHead };
  const bill = await attempt(() => scopedBill(bundleRaw, args.hash ? inspect(manifestRaw).replace(["contentHash"], JSON.stringify(args.hash)) : manifestRaw, headRaw, keyJson, api));
  if (!report("bill: hash, inclusion, charge, price, sale", bill.ok, bill.ok ? `${bill.charge}${bill.price}${bill.sale ?? ""}` : bill.detail ?? bill.verdict)) {
    failures += 1;
  }

  const signed = await attempt(() =>
    verifySignedHead(head.note, servedHead, manifest.origin, keyJson),
  );
  if (!report("head: operator signature", signed.ok, signed.detail)) {
    failures += 1;
  }

  const chained = await attempt(() =>
    scopedConsistency(readFileSync(join(dir, "consistency.json"), "utf8"), readFileSync(join(dir, "held-head.json"), "utf8"), headRaw, manifest.origin, keyJson, api),
  );
  if (!report("consistency: the new head extends the archived head", chained.ok, chained.detail)) {
    failures += 1;
  }

  const notes = (checkpoint.data?.cosignatures ?? []).map(({ witnessUrl, note }) => ({
    witnessUrl,
    note,
  }));
  if (notes.length === 0) {
    console.log("[SKIP] witnesses: no cosigned checkpoint — this deployment pins no witnesses");
    console.log("       The bill and head checks above do not depend on this line.");
  } else {
    const pins = checkpoint.data.witnessKeys ?? [];
    const witnesses = await attempt(() =>
      verifyWitnesses(
        JSON.stringify(notes),
        JSON.stringify(pins),
        manifest.origin,
        servedHead,
      ),
    );
    const allOk = witnesses.ok && witnesses.every((witness) => witness.ok);
    if (!report("witnesses: cosignatures against the pinset", allOk, witnesses.ok ? witnesses.map((w) => `${w.witnessUrl}: ${w.detail}`).join("; ") : witnesses.detail)) {
      failures += 1;
    }
  }

  if (evidence) {
    console.log();
    console.log("== Version-2 evidence bundle ==");
    console.log(`bundle ${evidenceManifest.bundleId}`);
    console.log(`organization ${evidenceManifest.organizationId}`);
    console.log(`ledger ${evidenceManifest.origin}`);
    console.log();

    // What the bundle attests before any check runs: the committed
    // (kind, id) manifest — pure inspection, no verification.
    const manifestRows = listBundleManifests(evidenceRaw);
    console.log(`manifest: ${manifestRows.length} committed artifact(s)`);
    for (const row of manifestRows) {
      console.log(`       ${row.kind}/${row.id}  ${row.digest.slice(0, 16)}…`);
    }
    console.log();

    const evidenceKeyJson = JSON.stringify(evidenceKey);
    const bundleJson = evidenceRaw;
    const evidenceManifestRaw = readFileSync(join(dir, "evidence-manifest.json"), "utf8");
    const checked = await attempt(() =>
      scopedEvidence(bundleJson, evidenceManifestRaw, evidenceKeyJson, api),
    );
    if (
      !report(
        "evidence bundle: manifest, artifacts, entries, signed head",
        checked.ok,
        checked.detail,
      )
    ) {
      failures += 1;
    }

    // Mutate an actually committed artifact while preserving all other raw
    // numeric tokens; an empty scope records the unavailable row explicitly.
    const firstRow = evidence.manifest?.[0];
    if (firstRow && inspect(bundleJson).has(["artifacts", firstRow.kind, firstRow.id])) {
      const view = inspect(bundleJson);
      const path = ["artifacts", firstRow.kind, firstRow.id];
      const replacement = view.token(path) === "null" ? "{}" : "null";
      const changed = view.replace(path, replacement);
      const altered = await attempt(() => verifyEvidenceBundle(changed, evidenceManifest.organizationId, evidenceKeyJson));
      if (!expectRejected("a changed committed artifact", altered)) failures += 1;
    } else {
      console.log("[SKIP] artifact tamper: the exported scope has no committed artifact");
    }

    // A dropped manifest row: the bundle no longer serves every artifact the
    // ledger committed.
    if ((evidence.manifest ?? []).length) {
      const droppedJson = inspect(bundleJson).replace(["manifest"], "[]");
      const droppedVerdict = await attempt(() => verifyEvidenceBundle(droppedJson, evidenceManifest.organizationId, evidenceKeyJson));
      if (!expectRejected("a missing committed row", droppedVerdict)) failures += 1;
      const row = evidence.manifest[0];
      const missingJson = inspect(bundleJson).replace(["artifacts", row.kind], "{}");
      const missingVerdict = await attempt(() => verifyEvidenceBundle(missingJson, evidenceManifest.organizationId, evidenceKeyJson));
      if (!expectRejected("a missing committed artifact", missingVerdict)) failures += 1;
    } else {
      console.log("[SKIP] missing committed row: the exported scope is empty");
    }
    const wrongOrigin = inspect(evidenceManifestRaw).replace(["origin"], JSON.stringify(evidenceManifest.origin+"/other"));
    if (!expectRejected("evidence from a different retained origin", await attempt(() => scopedEvidence(bundleJson, wrongOrigin, evidenceKeyJson, api)))) failures += 1;
    const wrongEvidenceKey = { ...evidenceKey, publicKey: "00".repeat(32) };
    if (!expectRejected("evidence with a different retained key", await attempt(() => scopedEvidence(bundleJson, evidenceManifestRaw, JSON.stringify(wrongEvidenceKey), api)))) failures += 1;

    // A bundle for a different customer fails closed — the manifest's
    // organizationId is part of what the marker committed.
    const wrongOrgVerdict = await attempt(() =>
      verifyEvidenceBundle(bundleJson, "not-the-customer", evidenceKeyJson),
    );
    if (!expectRejected("a bundle for a different customer", wrongOrgVerdict)) {
      failures += 1;
    }
  } else {
    console.log();
    console.log("[SKIP] evidence bundle: no evidence-bundle.json fixture in this directory");
  }

  console.log();
  console.log("== Rejection checks (altered inputs) ==");

  const billView = inspect(bundleRaw);
  const originalAmount = billView.get(["entry", "postings", 0, "amount"]);
  const changedAmount = originalAmount === "0.003001" ? "0.003002" : "0.003001";
  const tampered = billView.replace(["entry", "postings", 0, "amount"], JSON.stringify(changedAmount));
  const tamperedVerdict = await attempt(() =>
    verifyBill(tampered, expectedHash),
  );
  if (!expectRejected("tampered bundle content", tamperedVerdict)) {
    failures += 1;
  }

  for (const binding of ["price", "sale"]) {
    if (!billView.has([binding]) || billView.get([binding]) === null) {
      console.log(`[SKIP] ${binding} mutation: no such binding in this legacy bill`);
      continue;
    }
    const bindingRow = billView.get([binding, "row"]);
    const changed = billView.replace([binding, "row"], JSON.stringify(bindingRow+" "));
    if (!expectRejected(`a changed ${binding} catalog row`, await attempt(() => scopedBill(changed, manifestRaw, headRaw, keyJson, api)))) failures += 1;
    const missing = billView.replace([binding], "null");
    const missingVerdict = await attempt(() => scopedBill(missing, manifestRaw, headRaw, keyJson, api));
    const salePriced = inspect(billView.get(["entry", "description"])).has(["sale", "digest"]);
    if (binding === "price" && salePriced) {
      if (!report("sale-priced bill remains verifiable without optional wholesale binding", missingVerdict.ok, missingVerdict.detail)) failures += 1;
    } else if (!expectRejected(`a missing required ${binding} binding`, missingVerdict)) failures += 1;
  }

  const description = inspect(billView.get(["entry", "description"]));
  if (description.has(["lines", 0, 2])) {
    const rate = description.token(["lines", 0, 2]);
    const changedDescription = description.replace(["lines", 0, 2], (BigInt(rate) + 1n).toString());
    const changedRate = billView.replace(["entry", "description"], JSON.stringify(changedDescription));
    if (!expectRejected("a changed recorded rate", await attempt(() => scopedBill(changedRate, manifestRaw, headRaw, keyJson, api)))) failures += 1;
  } else console.log("[SKIP] recorded rate mutation: this record has no rated lines");

  const wrongHash = (expectedHash[0] === "0" ? "1" : "0") + expectedHash.slice(1);
  const wrongHashVerdict = await attempt(() => verifyBill(bundleRaw, wrongHash));
  if (!expectRejected("a different customer-held hash", wrongHashVerdict)) {
    failures += 1;
  }

  const wrongKey = { ...key, publicKey: (key.publicKey[0] === "A" ? "B" : "A") + key.publicKey.slice(1) };
  const wrongKeyVerdict = await attempt(() =>
    verifySignedHead(head.note, servedHead, manifest.origin, JSON.stringify(wrongKey)),
  );
  if (!expectRejected("a different operator key", wrongKeyVerdict)) {
    failures += 1;
  }

  const wrongHeld = { size: held.size, root: "00".repeat(32) };
  const wrongHeldVerdict = await attempt(() =>
    verifyConsistency(readFileSync(join(dir, "consistency.json"), "utf8"), JSON.stringify(wrongHeld), manifest.origin, keyJson),
  );
  if (!expectRejected("a different archived head", wrongHeldVerdict)) {
    failures += 1;
  }

  console.log();
  if (failures > 0) {
    console.error(`${failures} check(s) did not behave as expected.`);
    process.exit(1);
  }
  if (networkAttempts > 0) {
    console.error(`the verifier attempted ${networkAttempts} network request(s).`);
    process.exit(1);
  }
  console.log("All checks behaved as expected. fetch was disabled and never called.");
}

await main();
