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

import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  verifyBill,
  verifyConsistency,
  verifyEvidenceBundle,
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
  const manifest = readJson(join(dir, "manifest.json"));
  const bundle = readJson(join(dir, "settlement-bundle.json"));
  const head = readJson(join(dir, "signed-head.json"));
  const key = readJson(join(dir, "operator-key.json"));
  const held = readJson(join(dir, "held-head.json"));
  const consistency = readJson(join(dir, "consistency.json"));
  const checkpoint = readJson(join(dir, "checkpoint.json"));
  const expectedHash = args.hash ?? manifest.contentHash;

  // The v2 evidence-bundle fixture: a complete-period bundle minted for one
  // organization, the published operator key it carries, and the manifest
  // naming the organization and ledger origin the check binds. Optional — a
  // fixtures dir without it simply skips the v2 block.
  const evidencePath = join(dir, "evidence-bundle.json");
  const evidence = existsSync(evidencePath) ? readJson(evidencePath) : null;
  const evidenceManifest = evidence
    ? readJson(join(dir, "evidence-manifest.json"))
    : null;
  const evidenceKey = evidence
    ? readJson(join(dir, "evidence-operator-key.json"))
    : null;

  const servedHead = JSON.stringify({ size: head.size, root: head.root });
  const keyJson = JSON.stringify(key);
  let failures = 0;

  console.log("== Genuine exported evidence ==");
  console.log(`entry ${manifest.entryId}`);
  console.log(`content hash ${expectedHash}`);
  console.log(`ledger ${manifest.origin}`);
  console.log();

  const bill = await attempt(() => verifyBill(JSON.stringify(bundle), expectedHash));
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
    verifyConsistency(JSON.stringify(consistency), JSON.stringify(held), manifest.origin, keyJson),
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

    const evidenceKeyJson = JSON.stringify(evidenceKey);
    const bundleJson = JSON.stringify(evidence);
    const checked = await attempt(() =>
      verifyEvidenceBundle(bundleJson, evidenceManifest.organizationId, evidenceKeyJson),
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

    // A tampered artifact: the usage figure no longer digests to what the
    // manifest committed.
    const tamperedArtifact = structuredClone(evidence);
    for (const bucket of Object.values(tamperedArtifact.artifacts ?? {})) {
      for (const artifact of Object.values(bucket ?? {})) {
        if (artifact && typeof artifact === "object" && "chargedMinor" in artifact) {
          artifact.chargedMinor += 1;
        }
      }
    }
    const tamperedVerdict = await attempt(() =>
      verifyEvidenceBundle(
        JSON.stringify(tamperedArtifact),
        evidenceManifest.organizationId,
        evidenceKeyJson,
      ),
    );
    if (!expectRejected("a tampered committed artifact", tamperedVerdict)) {
      failures += 1;
    }

    // A dropped manifest row: the bundle no longer serves every artifact the
    // ledger committed.
    const dropped = structuredClone(evidence);
    dropped.manifest = (dropped.manifest ?? []).slice(1);
    const droppedVerdict = await attempt(() =>
      verifyEvidenceBundle(
        JSON.stringify(dropped),
        evidenceManifest.organizationId,
        evidenceKeyJson,
      ),
    );
    if (!expectRejected("a missing committed row", droppedVerdict)) {
      failures += 1;
    }

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

  const tampered = structuredClone(bundle);
  tampered.entry.postings[0].amount = "0.003001";
  const tamperedVerdict = await attempt(() =>
    verifyBill(JSON.stringify(tampered), expectedHash),
  );
  if (!expectRejected("tampered bundle content", tamperedVerdict)) {
    failures += 1;
  }

  const wrongHash = (expectedHash[0] === "0" ? "1" : "0") + expectedHash.slice(1);
  const wrongHashVerdict = await attempt(() => verifyBill(JSON.stringify(bundle), wrongHash));
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
    verifyConsistency(JSON.stringify(consistency), JSON.stringify(wrongHeld), manifest.origin, keyJson),
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
