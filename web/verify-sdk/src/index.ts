/**
 * The wasm-bindgen module `scripts/build-verify-wasm.sh` emits. The SDK ships it
 * inside `wasm/` (see `files` in package.json) so the whole thing — loader plus
 * the `.wasm` it drives — is one npm package, not a URL the host page must
 * also serve. The `verify_wasm.js` glue is a *module*: the default export is
 * the async init that instantiates the inner `.wasm`, and the named exports
 * are the `#[wasm_bindgen]` functions the crate surfaces.
 */

/** The `{"ok": bool, "detail": string}` shape every exported verdict shares. */
export interface Verdict {
  ok: boolean;
  detail: string;
}

/** `verifyBundle`'s answer — the bill verdict plus the charge/price prose. */
export interface BillVerdict {
  /** `verdict === "pass"`. */
  ok: boolean;
  /** `"pass"`, `"chargeMismatch"` or `"fail"` — the machine name `ok` mirrors. */
  verdict: "pass" | "chargeMismatch" | "fail" | string;
  charge?: string;
  price?: string;
  /** Present only when the settlement committed a `sale` digest (issue #600). */
  sale?: string | null;
}

/** `verifySignedHead` / `verifyConsistency` answer this. */
export interface HeadVerdict extends Verdict {}

/** One witness's answer from `verifyWitnesses`, in the notes' own order. */
export interface WitnessVerdict {
  /** The witness endpoint the note came from. */
  witnessUrl: string;
  ok: boolean;
  /** The same human-readable sentence the Rust verifier prints. */
  detail: string;
}

/** `verifyStatementAnchor` / `verifyUsageReport` / `verifyPriceHead` answer this. */
export interface CheckVerdict extends Verdict {}

/**
 * The published operator key a signature check needs:
 * `{keyName, publicKey, keyHash}`. `publicKey` may be the base64 form the
 * `/api/v1/log/*` endpoints serve or the hex form the proof/report bundles
 * carry; the wrapper normalises it. Obtain the key through a channel the
 * operator does not control; a key fetched from the same server only proves
 * the server agrees with itself.
 */
export interface PublishedKey {
  keyName: string;
  publicKey: string;
  keyHash: string;
}

interface VerifyWasmModule {
  verifyBundle(bundleJson: string, expectedHashHex: string): Promise<string>;
  verifySignedHead(
    note: string,
    servedHeadJson: string,
    expectedOrigin: string,
    keyJson: string,
  ): Promise<string>;
  verifyConsistency(
    responseJson: string,
    heldHeadJson: string,
    expectedOrigin: string,
    keyJson: string,
  ): Promise<string>;
  verifyWitnesses(
    notesJson: string,
    pinsJson: string,
    expectedOrigin: string,
    headJson: string,
  ): Promise<string>;
  verifyStatementAnchor(json: string, checkJson: string): Promise<string>;
  verifyUsageReport(
    json: string,
    expectedEndUser: string,
    keyJson?: string | null,
  ): Promise<string>;
  verifyPriceHead(json: string): Promise<string>;
  verifySaleHead(json: string): Promise<string>;
  verifyEvidenceBundle(
    json: string,
    expectedOrganization: string,
    keyJson?: string | null,
  ): Promise<string>;
}

let modulePromise: Promise<VerifyWasmModule> | null = null;

/**
 * Lazily instantiates the embedded wasm module — the import of
 * `wasm/verify_wasm.js` triggers its default-export init, which fetches the
 * sibling `.wasm` and returns the bound exports. Called once per verifier; the
 * promise is memoized so the module loads a single time per page.
 */
async function load(): Promise<VerifyWasmModule> {
  if (!modulePromise) {
    modulePromise = (async () => {
      // wasm/verify_wasm.js is the wasm-bindgen `--target web` output — its
      // default export is the async initializer the glue installs, and its
      // named exports are the JS wrappers that take and return JSON strings.
      // The initializer's own return value is the raw
      // `WebAssembly.Instance.exports`, whose functions speak pointers, not
      // JSON: the wrappers must come from the module namespace (issue #555).
      const glue = (await import(
        /* @vite-ignore */ new URL("../wasm/verify_wasm.js", import.meta.url).href
      )) as { default: (input?: unknown) => Promise<unknown> } & VerifyWasmModule;
      // The glue's default path fetches the sibling `.wasm` — right in a
      // browser, unavailable under Node's `file:` URLs. Give it the bytes
      // instead when running under Node (process.getBuiltinModule is
      // invisible to browser bundlers, unlike an import of `node:fs`).
      const runtime = (
        globalThis as {
          process?: {
            versions?: { node?: string };
            getBuiltinModule?: (id: string) => unknown;
          };
        }
      ).process;
      if (runtime?.versions?.node) {
        if (!runtime.getBuiltinModule) {
          throw new Error("verify-oxsum-bill requires Node 20.16 or newer");
        }
        const fs = runtime.getBuiltinModule("node:fs/promises") as {
          readFile: (path: URL) => Promise<Uint8Array>;
        };
        const bytes = await fs.readFile(
          new URL("../wasm/verify_wasm_bg.wasm", import.meta.url),
        );
        await glue.default({ module_or_path: bytes });
      } else {
        await glue.default();
      }
      return glue;
    })();
  }
  return modulePromise;
}

function parse<T extends object>(json: string): T {
  return JSON.parse(json) as T;
}

/**
 * The Rust verifier reads Ed25519 public keys as lowercase hex. The signed-head
 * endpoints serve the operator key as base64 (C2SP's own spelling), while the
 * proof/report bundles carry hex — normalise both to hex so a caller can pass
 * whichever form it obtained. A value that is neither is left alone; the
 * verifier reports it as malformed.
 */
function publicKeyHex(value: string): string {
  if (/^[0-9a-f]{64}$/i.test(value)) {
    return value.toLowerCase();
  }
  try {
    const binary = atob(value);
    if (binary.length !== 32) {
      return value;
    }
    let hex = "";
    for (let i = 0; i < binary.length; i += 1) {
      hex += binary.charCodeAt(i).toString(16).padStart(2, "0");
    }
    return hex;
  } catch {
    return value;
  }
}

/** A `{keyName, publicKey, keyHash}` JSON string with `publicKey` in hex. */
function normalizedKeyJson(key: string): string {
  const parsed = JSON.parse(key) as { publicKey?: unknown };
  if (typeof parsed.publicKey === "string") {
    parsed.publicKey = publicKeyHex(parsed.publicKey);
  }
  return JSON.stringify(parsed);
}

/** A `[{witnessUrl, keyName, publicKey, keyHash}]` array with hex keys. */
function normalizedPinsJson(pins: string): string {
  const parsed = JSON.parse(pins) as { publicKey?: unknown }[];
  for (const pin of parsed) {
    if (typeof pin.publicKey === "string") {
      pin.publicKey = publicKeyHex(pin.publicKey);
    }
  }
  return JSON.stringify(parsed);
}

/**
 * Verifies a bill's proof bundle — recomputes the content hash from the entry
 * source, links it to the tree head, and (when the bundle carries them)
 * recomputes the charge and checks the signed price binding.
 *
 * @param bundleJson the `ProofBundle` JSON — or the `{"data": …}` envelope the
 *   `GET /api/v1/entries/{entryId}/proof` endpoint wraps it in.
 * @param expectedHashHex the 64-character content hash the user recorded with
 *   the bill.
 */
export async function verifyBill(
  bundleJson: string,
  expectedHashHex: string,
): Promise<BillVerdict> {
  const m = await load();
  return parse(await m.verifyBundle(bundleJson, expectedHashHex));
}

/**
 * Checks that a signed tree head is genuine — the operator's Ed25519
 * signed-note signature over `oxsum/ledgers/<tenant>`.
 *
 * @param note the C2SP signed-note text verbatim (`data.note` from
 *   `GET /api/v1/log/head`), never a re-rendered copy: the signature covers
 *   the note's body byte for byte.
 * @param servedHeadJson the `{size, root}` head the note must attest — the
 *   `data.head` the caller read, or a head the caller already holds.
 * @param expectedOrigin the ledger origin the note must name, for example
 *   `oxsum/ledgers/<tenantId>`. Pass the one you recorded, not one taken from
 *   the same answer you are checking.
 * @param keyJson the published key `{keyName, publicKey, keyHash}`.
 */
export async function verifySignedHead(
  note: string,
  servedHeadJson: string,
  expectedOrigin: string,
  keyJson: string,
): Promise<HeadVerdict> {
  const m = await load();
  return parse(await m.verifySignedHead(note, servedHeadJson, expectedOrigin, normalizedKeyJson(keyJson)));
}

/**
 * Checks that a new head extends a head the verifier already holds — the
 * consistency proof an archived head demands.
 *
 * @param responseJson the `data` of `GET /api/v1/log/consistency`: the new
 *   signed note plus `oldHead`, `head` and `proof`.
 * @param heldHeadJson the `{size, root}` head the caller archived. The
 *   response's `oldHead` must match it exactly; a proof anchored anywhere
 *   else proves nothing about the caller's archive.
 * @param expectedOrigin the ledger origin the note must name.
 * @param keyJson the published operator key `{keyName, publicKey, keyHash}`.
 */
export async function verifyConsistency(
  responseJson: string,
  heldHeadJson: string,
  expectedOrigin: string,
  keyJson: string,
): Promise<HeadVerdict> {
  const m = await load();
  return parse(await m.verifyConsistency(responseJson, heldHeadJson, expectedOrigin, normalizedKeyJson(keyJson)));
}

/**
 * Checks every witness cosignature on a checkpoint against a pinset. A
 * failure is reported in-band per witness, not thrown: the element and the
 * example can say which witness failed and why.
 *
 * @param notesJson `[{witnessUrl, note}]` — the checkpoint's cosignatures.
 * @param pinsJson `[{witnessUrl, keyName, publicKey, keyHash}]` — the pins the
 *   caller trusts, not necessarily the ones the deployment serves.
 * @param expectedOrigin the ledger origin the notes must name.
 * @param headJson the `{size, root}` head the notes must attest.
 */
export async function verifyWitnesses(
  notesJson: string,
  pinsJson: string,
  expectedOrigin: string,
  headJson: string,
): Promise<WitnessVerdict[]> {
  const m = await load();
  return parse<WitnessVerdict[]>(await m.verifyWitnesses(notesJson, normalizedPinsJson(pinsJson), expectedOrigin, headJson));
}

/** Verifies a statement's completeness anchor against its check payload. */
export async function verifyStatementAnchor(
  json: string,
  checkJson: string,
): Promise<CheckVerdict> {
  const m = await load();
  return parse(await m.verifyStatementAnchor(json, checkJson));
}

/**
 * Verifies a usage-report bundle against the ledger subset its marker commits.
 *
 * @param json the report bundle `data` object.
 * @param expectedEndUser who the report should be for — the caller-supplied
 *   recipient name; a bundle committing a different `endUser` fails.
 * @param keyJson the published operator key `{keyName, publicKey, keyHash}` —
 *   or `null` to check membership to the committed digests without a signature.
 */
export async function verifyUsageReport(
  json: string,
  expectedEndUser: string,
  keyJson?: string | null,
): Promise<CheckVerdict> {
  const m = await load();
  return parse(await m.verifyUsageReport(json, expectedEndUser, keyJson ? normalizedKeyJson(keyJson) : keyJson));
}

/** Verifies a signed price-head checkpoint against the row it attests. */
export async function verifyPriceHead(json: string): Promise<CheckVerdict> {
  const m = await load();
  return parse(await m.verifyPriceHead(json));
}

/**
 * Verifies a bundle's signed sale binding — the customer sale terms a
 * sale-priced settlement committed (issue #600). `json` is the `ProofBundle`
 * (or its envelope) carrying `entry` and `sale`.
 */
export async function verifySaleHead(json: string): Promise<CheckVerdict> {
  const m = await load();
  return parse(await m.verifySaleHead(json));
}

/**
 * Verifies a version-2 customer evidence bundle (issue #616): every artifact
 * the manifest commits re-digests to the figure the ledger's marker bound, the
 * money artifacts' ledger entries include in the signed head, and the head's
 * signature verifies. The ledger origin is read out of the bundle's own signed
 * note — the bundle names its tenant, so a bundle minted elsewhere answers
 * under its own origin rather than silently inheriting a caller-supplied one.
 *
 * @param json the bundle `data` object — what
 *   `GET /api/v1/org/evidence-bundles/{bundleId}` or the public
 *   `GET /api/v1/evidence/{token}` answers.
 * @param expectedOrganization the customer the bundle should be for — a bundle
 *   minted for another organization fails closed.
 * @param keyJson the published operator key `{keyName, publicKey, keyHash}` —
 *   or `null` to check the digests and inclusion alone, which is what an
 *   unsigned deployment leaves verifiable.
 */
export async function verifyEvidenceBundle(
  json: string,
  expectedOrganization: string,
  keyJson?: string | null,
): Promise<CheckVerdict> {
  const m = await load();
  return parse(
    await m.verifyEvidenceBundle(
      json,
      expectedOrganization,
      keyJson ? normalizedKeyJson(keyJson) : keyJson,
    ),
  );
}

/** One row of a version-2 evidence bundle's committed manifest. */
export interface BundleManifestRow {
  /** The artifact class — `usageRecord`, `paymentOrder`, `daySeal`, … */
  kind: string;
  /** The artifact's own id inside its kind bucket. */
  id: string;
  /** The lowercase-hex digest the manifest committed for this artifact. */
  digest: string;
}

/**
 * Reads a version-2 evidence bundle's manifest — the `(kind, id)`-sorted list
 * of every artifact the ledger marker committed, each with the digest the
 * bundle's served bytes must re-digest to. Pure inspection: it parses the
 * bundle JSON and returns the manifest rows verbatim, so a developer can list
 * what a bundle attests before or instead of running the full verifier.
 *
 * The function never touches `artifacts` or `entries`: a bundle whose
 * `manifest` is not an array of `{kind, id, digest}` strings is malformed and
 * throws, matching the verifier's own refusal to guess.
 *
 * @param json the bundle `data` object — the same JSON
 *   `verifyEvidenceBundle` consumes.
 */
export function listBundleManifests(json: string): BundleManifestRow[] {
  const parsed = JSON.parse(json) as { manifest?: unknown };
  const manifest = parsed.manifest;
  if (!Array.isArray(manifest)) {
    throw new Error("evidence bundle carries no manifest array");
  }
  return manifest.map((row) => {
    const { kind, id, digest } = (row ?? {}) as Record<string, unknown>;
    if (
      typeof kind !== "string" ||
      typeof id !== "string" ||
      typeof digest !== "string"
    ) {
      throw new Error("evidence bundle manifest row is not {kind, id, digest}");
    }
    return { kind, id, digest };
  });
}
