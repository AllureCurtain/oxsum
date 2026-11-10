/** Offline verifier; install the candidate tarball separately in this directory. */
import { readFileSync, existsSync } from "node:fs";
import { resolve, join } from "node:path";
import { scopedBill, scopedEvidence } from "./scope.mjs";
import * as api from "verify-oxsum-bill";
globalThis.fetch = () => { throw new Error("offline verification cannot fetch"); };
const directory = resolve(process.argv[2] ?? "exports");
const read = name => readFileSync(join(directory, name), "utf8");
const key = read("operator-key.json");
if (existsSync(join(directory, "settlement-bundle.json"))) {
  await scopedBill(read("settlement-bundle.json"), read("manifest.json"), read("signed-head.json"), key, api);
  console.log("PASS: retained receipt hash, charge, catalog bindings and signed origin");
}
if (existsSync(join(directory, "evidence-bundle.json"))) {
  await scopedEvidence(read("evidence-bundle.json"), read("evidence-manifest.json"), key, api);
  console.log("PASS: retained customer, bundle digest, manifest and signed origin");
}
if (!existsSync(join(directory, "settlement-bundle.json")) && !existsSync(join(directory, "evidence-bundle.json"))) throw new Error("No complete export found");
