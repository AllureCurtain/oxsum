/** Frozen evidence selection and local verification; no same-server response is
 * promoted into an independently trusted key or a wider proof claim. */
import type { components } from "../api/schema";
import { readArchive, type HeadVerifier, type LedgerIdentity, type PublishedKey, type RetainedHead } from "./retained-head.ts";

export type EvidenceBundle = components["schemas"]["EvidenceBundle"];
export type EvidenceSummary = components["schemas"]["EvidenceBundleSummary"];
export interface EvidenceVerifier extends HeadVerifier {
  verifyEvidenceBundle(json: string, organizationId: string, key?: string | null): string | Promise<string>;
}
export interface EvidenceExpected {
  organizationId: string;
  origin: string;
  bundleId: string;
  manifestDigest: string;
}
export interface EvidenceResult {
  signature: "retainedKey" | "suppliedKey" | "unsigned";
  expected: EvidenceExpected;
  key: PublishedKey | null;
}

/** Inclusive UTC days, matching the server's ended-span and 366-day contract. */
export function evidencePeriod(from: string, to: string, now = Date.now()): { from: string; to: string } {
  function day(value: string): number {
    const parsed = Date.parse(`${value}T00:00:00.000Z`);
    if (!/^\d{4}-\d{2}-\d{2}$/.test(value) || !Number.isFinite(parsed) || new Date(parsed).toISOString().slice(0, 10) !== value) throw new Error("Use valid UTC calendar dates.");
    return parsed;
  }
  const start = day(from); const end = day(to);
  const today = day(new Date(now).toISOString().slice(0, 10));
  if (end < start || end >= today || (end - start) / 86_400_000 + 1 > 366) throw new Error("Select an ended UTC span of 1 to 366 days.");
  return { from, to };
}

export function expectedEvidence(identity: LedgerIdentity, selected: Pick<EvidenceSummary, "bundleId" | "manifestDigest">): EvidenceExpected {
  return { organizationId: identity.organizationId, origin: `oxsum/ledgers/${identity.tenantId}`, bundleId: selected.bundleId, manifestDigest: selected.manifestDigest };
}

/** List/mint metadata is the user's selected scope. Its dates are not implied by
 * merely matching a digest, so bind them independently before publishing a result. */
export function matchesEvidence(summary: EvidenceSummary, bundle: EvidenceBundle): boolean {
  return summary.bundleId === bundle.bundleId && summary.manifestDigest === bundle.manifestDigest && summary.kind === bundle.kind
    && (summary.periodFrom ?? null) === (bundle.periodFrom ?? null)
    && (summary.periodTo ?? null) === (bundle.periodTo ?? null)
    && (summary.statementId ?? null) === (bundle.statementId ?? null);
}

function passed(json: string): void {
  if ((JSON.parse(json) as { ok?: unknown })?.ok !== true) throw new Error("The local verifier did not confirm the evidence.");
}

export async function verifyEvidence(json: string, expected: EvidenceExpected, identity: LedgerIdentity, retained: RetainedHead | null, verifier: EvidenceVerifier): Promise<EvidenceResult> {
  const bundle = JSON.parse(json) as EvidenceBundle;
  if (bundle.version !== 2 || bundle.organizationId !== identity.organizationId || expected.organizationId !== identity.organizationId || expected.origin !== `oxsum/ledgers/${identity.tenantId}` || bundle.bundleId !== expected.bundleId || bundle.manifestDigest !== expected.manifestDigest) throw new Error("The evidence differs from the selected customer or retained manifest.");
  const stored = retained ? readArchive(JSON.stringify(retained), identity) : null;
  const note = bundle.note;
  if (note !== undefined && note !== null && (typeof note !== "string" || note.length === 0)) throw new Error("The evidence signature is malformed.");
  let key: PublishedKey | null = stored?.key ?? null;
  let signature: EvidenceResult["signature"] = "unsigned";
  if (note) {
    if (!key) {
      if (!bundle.keyName || !bundle.publicKey || !bundle.keyHash || !/^[0-9a-f]{64}$/.test(bundle.publicKey)) throw new Error("The evidence operator key is incomplete.");
      key = { keyName: bundle.keyName, publicKey: bundle.publicKey, keyHash: bundle.keyHash };
    }
    if (stored) passed(await verifier.verifySignedHead(stored.note, JSON.stringify(stored.head), stored.origin, JSON.stringify(stored.key)));
    // The WASM evidence wrapper derives its origin from the note itself. Bind
    // this separately to the authenticated customer's ledger identity.
    passed(await verifier.verifySignedHead(note, JSON.stringify(bundle.head), expected.origin, JSON.stringify(key)));
    signature = stored ? "retainedKey" : "suppliedKey";
  }
  passed(await verifier.verifyEvidenceBundle(json, identity.organizationId, key ? JSON.stringify(key) : null));
  return { signature, expected: { ...expected }, key };
}

/** Keep the original data bytes: parsing and reserializing money can round integers
 * above JavaScript's safe range and invalidate otherwise sound evidence. */
export function evidenceBody(envelope: string): { bundle: EvidenceBundle; json: string } {
  const parsed = JSON.parse(envelope) as { data?: EvidenceBundle };
  const match = /^\s*\{\s*"data"\s*:\s*([\s\S]+)\}\s*$/.exec(envelope);
  if (!parsed || Object.keys(parsed).length !== 1 || !parsed.data || !match) throw new Error("The evidence response envelope is malformed.");
  const raw = JSON.parse(match[1]);
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) throw new Error("The evidence body is malformed.");
  return { bundle: parsed.data, json: match[1] };
}

/** Download JSON without navigating to a live API or exposing a bearer capability. */
export function downloadEvidence(value: unknown, filename: string): void {
  const url = URL.createObjectURL(new Blob([typeof value === "string" ? value : JSON.stringify(value, null, 2) + "\n"], { type: "application/json" }));
  const anchor = document.createElement("a");
  anchor.href = url; anchor.download = filename; anchor.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
