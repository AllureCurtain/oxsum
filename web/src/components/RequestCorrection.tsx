import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { api, ApiError } from "../api/client";
import type { components } from "../api/schema";
import { useBusiness } from "../api/hooks";
import { AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogHeader, AlertDialogTitle, Banner, Button, Field, TextInput } from "./ui";

type Preview = components["schemas"]["RequestCorrectionPreview"];
type Credit = components["schemas"]["RequestCorrection"];
type Intent = components["schemas"]["RequestCorrectionInput"];
interface Retained { preview: Preview; request: Intent }
interface Props { customerId: string; sourceEntryId: string; onApplied: () => void }

/** Recover exact uncertain intents across reload, scoped to staff and source. */
export function RequestCorrection(props: Props) {
  const business = useBusiness().data;
  if (!business?.capabilities.includes("fundsWrite")) return null;
  const storageKey = `request-credit:${business.businessId}:${business.userId}:${props.customerId}:${props.sourceEntryId}`;
  return <CorrectionForm key={storageKey} {...props} storageKey={storageKey} />;
}

function CorrectionForm({ customerId, sourceEntryId, onApplied, storageKey }: Props & { storageKey: string }) {
  const [open, setOpen] = useState(false);
  const [amount, setAmount] = useState("");
  const [reason, setReason] = useState("");
  const [localError, setLocalError] = useState("");
  const [result, setResult] = useState<Credit>();
  const [intent, setIntent] = useState<Retained | null>(() => {
    try { const saved = JSON.parse(sessionStorage.getItem(storageKey) ?? "null") as Retained | null;
      return saved?.request.sourceEntryId === sourceEntryId && typeof saved.request.idempotencyKey === "string" ? saved : null;
    } catch { return null; }
  });
  const base = `/business/customers/${customerId}/request-corrections`;
  const preview = useMutation({ mutationFn: () => api<Preview>(`${base}/preview?sourceEntryId=${sourceEntryId}`), onSuccess: p => { setAmount(String(p.remainingMinor)); setResult(undefined); } });
  const apply = useMutation({ mutationFn: (request: Intent) => api<Credit>(base, { method: "POST", body: request }),
    onSuccess: r => { sessionStorage.removeItem(storageKey); setIntent(null); setResult(r); preview.reset(); setOpen(false); onApplied(); },
    onError: error => {
      // Unknown network/server outcomes retain the exact reviewed body and key.
      if (error instanceof ApiError && [400,409].includes(error.status)) {
        sessionStorage.removeItem(storageKey); setIntent(null); preview.reset();
      }
    },
  });
  const reviewed = intent?.preview ?? preview.data;
  const busy = preview.isPending || apply.isPending;
  const valid = /^\d+$/.test(amount) && Number.isSafeInteger(Number(amount)) && Number(amount) > 0 && Number(amount) <= (reviewed?.remainingMinor ?? 0);
  function submit() {
    if (!reviewed || (!intent && (!valid || !reason.trim()))) return;
    const retained = intent ?? { preview: reviewed, request: { idempotencyKey: crypto.randomUUID(), sourceEntryId,
      sourceHash: reviewed.sourceHash, expectedCorrectedMinor: reviewed.correctedMinor, amountMinor: Number(amount), reason: reason.trim() } };
    try { sessionStorage.setItem(storageKey, JSON.stringify(retained)); }
    catch { setLocalError("Cannot retain this intent safely in browser storage. No request was sent."); return; }
    setIntent(retained); setLocalError(""); apply.mutate(retained.request);
  }
  return <div className="flex flex-col gap-4 border-t border-border pt-4">
    <Button disabled={busy} onClick={() => { if (intent) setOpen(true); else preview.mutate(); }}>{intent ? "Recover pending request credit" : "Preview request credit correction"}</Button>
    {preview.error ? <Banner tone="error">{preview.error.message}</Banner> : null}
    {result ? <Banner tone="success">Purchased wallet credited {result.amountMinor} minor units. Credit entry: <span className="break-all font-mono">{result.entryId}</span>.</Banner> : null}
    {reviewed && !intent ? <>
      <p>Original charge: <span className="font-mono">{reviewed.chargedMinor}</span>; already credited: <span className="font-mono">{reviewed.correctedMinor}</span>; remaining: <span className="font-mono">{reviewed.remainingMinor}</span> minor units.</p>
      <p>{reviewed.effect}. The original charge and its proof remain unchanged.</p>
      <Field label="Purchased credit to return (integer minor units)"><TextInput disabled={busy} inputMode="numeric" value={amount} onChange={e => setAmount(e.target.value)} /></Field>
      <Button disabled={!valid || busy} onClick={() => setOpen(true)}>Review credit</Button>
    </> : null}
    <AlertDialog open={open} onOpenChange={next => { if (!busy) setOpen(next); }}>
      <AlertDialogContent><AlertDialogHeader><AlertDialogTitle>Confirm source-linked request credit</AlertDialogTitle>
        <AlertDialogDescription>Credit {intent?.request.amountMinor ?? amount} minor units to customer {customerId}, against charge {sourceEntryId}. {reviewed?.effect}.</AlertDialogDescription></AlertDialogHeader>
        <Field label="Customer-visible reason"><TextInput maxLength={500} disabled={busy || !!intent} value={intent?.request.reason ?? reason} onChange={e => setReason(e.target.value)} /></Field>
        {intent ? <Banner tone="warn">The exact submitted intent is retained. Recover it before starting another credit; a lost response may already have committed.</Banner> : null}
        {apply.error ? <Banner tone="error">{apply.error.message}</Banner> : null}
        {localError ? <Banner tone="error">{localError}</Banner> : null}
        <Button isLoading={apply.isPending} disabled={busy || !reviewed || (!intent && (!valid || !reason.trim()))} onClick={submit}>{intent ? "Recover exact credit" : "Append wallet credit"}</Button>
        <Button tone="ghost" disabled={busy} onClick={() => setOpen(false)}>Close</Button>
      </AlertDialogContent>
    </AlertDialog>
  </div>;
}
