import { useState, type FormEvent } from "react";
import { useBusiness, useCapability } from "../../api/hooks";
import { useBundles, useBundleMutation, type BundleOffer, type BundlePublish } from "../../api/hooks/bundles";
import { decimalMinor } from "../../lib/money";
import {
  AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogFooter,
  AlertDialogHeader, AlertDialogTitle, Badge, Banner, Button, Card, Cr, Field,
  NativeSelect, Skeleton, TextInput,
} from "../../components/ui";

function decimal(value: number, digits: number): string {
  const scale = 10n ** BigInt(digits);
  const exact = BigInt(value);
  return `${exact / scale}.${(exact % scale).toString().padStart(digits, "0")}`;
}

function Terms({ offer }: { offer: BundleOffer }) {
  const terms = offer.terms;
  return <dl className="grid grid-cols-1 gap-4 text-sm sm:grid-cols-3">
    <div><dt className="text-muted">Price</dt><dd className="mono text-right">{decimal(terms.displayAmountMinor, 2)} {terms.displayCurrency}</dd></div>
    <div><dt className="text-muted">Purchased credit</dt><dd className="text-right"><Cr minor={terms.purchasedMinor} /></dd></div>
    <div><dt className="text-muted">Promotional credit</dt><dd className="text-right"><Cr minor={terms.promoMinor} /></dd></div>
    <div className="sm:col-span-3"><dt className="text-muted">Promotion validity</dt><dd>{terms.promoLifetimeDays == null ? "No expiry" : `${terms.promoLifetimeDays} days after confirmed payment`}. A confirmed partial or full refund cancels remaining promotional credit without cash reimbursement.</dd></div>
  </dl>;
}

function BundleEditor({ offers, done }: { offers: BundleOffer[]; done: () => void }) {
  const mutation = useBundleMutation();
  const [base, setBase] = useState<BundleOffer | null>(null);
  const [name, setName] = useState("");
  const [currency, setCurrency] = useState("USD");
  const [price, setPrice] = useState("");
  const [purchased, setPurchased] = useState("");
  const [promo, setPromo] = useState("0");
  const [lifetime, setLifetime] = useState("");
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState<Omit<BundlePublish, "idempotencyKey"> | null>(null);
  const [discard, setDiscard] = useState(false);
  const latest = new Map<string, BundleOffer>();
  for (const offer of offers) {
    if ((latest.get(offer.bundleId)?.version ?? 0) < offer.version) latest.set(offer.bundleId, offer);
  }
  const submit = (event: FormEvent) => {
    event.preventDefault();
    const displayAmountMinor = decimalMinor(price, 2);
    const purchasedMinor = decimalMinor(purchased, 6);
    const promoMinor = decimalMinor(promo, 6, true);
    const promoLifetimeDays = lifetime.trim() === "" ? null : decimalMinor(lifetime, 0);
    if (displayAmountMinor == null || purchasedMinor == null || promoMinor == null ||
        BigInt(purchasedMinor) + BigInt(promoMinor) > BigInt(Number.MAX_SAFE_INTEGER) ||
        (lifetime.trim() !== "" && (promoLifetimeDays == null || promoLifetimeDays > 3650))) {
      setError("Enter a positive price with at most 2 decimals and positive purchased credit with at most 6 decimals. Promotional credit can be zero. Validity is blank or 1–3650 whole days; amounts must fit the supported exact range.");
      return;
    }
    setError(null);
    setConfirmation({ bundleId: base?.bundleId ?? null, expectedVersion: base?.version ?? 0,
      reason: reason.trim(), terms: { name: name.trim(), displayCurrency: currency,
        displayAmountMinor, purchasedMinor, promoMinor, promoLifetimeDays } });
  };
  const publish = async () => {
    if (!confirmation) return;
    try { await mutation.submit("/business/bundles", confirmation); done(); }
    catch (err) { setError(err instanceof Error ? err.message : "Publication failed. Retry the unchanged terms to recover the same publication."); }
  };
  return <Card title="Publish bundle terms">
    <form onSubmit={submit} className="flex flex-col gap-4">
      <Field label="Offer family" hint="Each publication creates an immutable version. Previous versions stay on sale until separately retired.">
        <NativeSelect value={base?.bundleId ?? ""} onChange={(e) => setBase(latest.get(e.target.value) ?? null)}>
          <option value="">New offer</option>{[...latest.values()].map((offer) => <option key={offer.bundleId} value={offer.bundleId}>{offer.terms.name} · latest version {offer.version}</option>)}
        </NativeSelect>
      </Field>
      {base && latest.get(base.bundleId)?.version !== base.version ? <Banner tone="warn">A newer version was published. Re-select the offer family to review the new revision before publishing.</Banner> : null}
      <Field label="Name"><TextInput value={name} onChange={(e) => setName(e.target.value)} maxLength={200} required /></Field>
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
        <Field label="Price"><TextInput inputMode="decimal" value={price} onChange={(e) => setPrice(e.target.value)} required /></Field>
        <Field label="Currency" hint="Three uppercase letters; the configured merchant must support it."><TextInput value={currency} onChange={(e) => setCurrency(e.target.value.toUpperCase())} pattern="[A-Z]{3}" maxLength={3} required /></Field>
        <Field label="Purchased credits"><TextInput inputMode="decimal" value={purchased} onChange={(e) => setPurchased(e.target.value)} required /></Field>
        <Field label="Promotional credits"><TextInput inputMode="decimal" value={promo} onChange={(e) => setPromo(e.target.value)} required /></Field>
        <Field label="Promotion validity in days" hint="Leave blank for no expiry. Payment starts the validity period."><TextInput inputMode="numeric" value={lifetime} onChange={(e) => setLifetime(e.target.value)} /></Field>
      </div>
      <Field label="Reason"><TextInput value={reason} onChange={(e) => setReason(e.target.value)} required maxLength={500} /></Field>
      {error ? <Banner tone="error">{error}</Banner> : null}
      <div className="flex gap-2"><Button type="submit" disabled={!name.trim() || !reason.trim()}>Review publication</Button><Button type="button" tone="ghost" onClick={() => setDiscard(true)}>Discard draft</Button></div>
    </form>
    <AlertDialog open={confirmation !== null} onOpenChange={(open) => { if (!open && !mutation.isPending) setConfirmation(null); }}>
      <AlertDialogContent><AlertDialogHeader><AlertDialogTitle>Publish {confirmation?.terms.name}?</AlertDialogTitle><AlertDialogDescription>This version becomes available for new purchases. Its price, purchased credit, promotion and validity cannot be edited after publication.</AlertDialogDescription></AlertDialogHeader>
        {confirmation ? <Terms offer={{ bundleId: base?.bundleId ?? "", bundleVersionId: "", version: (base?.version ?? 0) + 1, terms: confirmation.terms, publishedAt: "", retiredAt: null }} /> : null}
        {error ? <Banner tone="error">{error}</Banner> : null}
        <AlertDialogFooter><Button tone="ghost" disabled={mutation.isPending} onClick={() => setConfirmation(null)}>Keep editing</Button><Button isLoading={mutation.isPending} onClick={() => void publish()}>Publish version</Button></AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
    <AlertDialog open={discard} onOpenChange={setDiscard}><AlertDialogContent><AlertDialogHeader><AlertDialogTitle>Discard this bundle draft?</AlertDialogTitle><AlertDialogDescription>The unsaved name, amounts, validity and reason will be lost.</AlertDialogDescription></AlertDialogHeader><AlertDialogFooter><Button tone="ghost" onClick={() => setDiscard(false)}>Keep draft</Button><Button tone="danger" onClick={done}>Discard</Button></AlertDialogFooter></AlertDialogContent></AlertDialog>
  </Card>;
}

function BundleCard({ offer, canWrite }: { offer: BundleOffer; canWrite: boolean }) {
  const mutation = useBundleMutation();
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const retire = async () => {
    try { await mutation.submit(`/business/bundles/${offer.bundleVersionId}/retire`, { reason: reason.trim() }); setOpen(false); }
    catch (err) { setError(err instanceof Error ? err.message : "Retirement failed; retry to recover its result."); }
  };
  return <Card title={`${offer.terms.name} · version ${offer.version}`} actions={<div className="flex items-center gap-2"><Badge tone={offer.retiredAt ? "secondary" : "success"}>{offer.retiredAt ? "Retired" : "On sale"}</Badge>{canWrite && !offer.retiredAt ? <Button tone="ghost" onClick={() => setOpen(true)}>Retire</Button> : null}</div>}>
    <Terms offer={offer} /><p className="mt-4 text-xs text-muted">Published {offer.publishedAt}{offer.retiredAt ? ` · Retired ${offer.retiredAt}` : ""}</p>
    <AlertDialog open={open} onOpenChange={(value) => { if (!mutation.isPending) setOpen(value); }}><AlertDialogContent><AlertDialogHeader><AlertDialogTitle>Retire {offer.terms.name}, version {offer.version}?</AlertDialogTitle><AlertDialogDescription>Customers cannot create new orders for this version. Orders already created keep their original payment and grant terms.</AlertDialogDescription></AlertDialogHeader><Field label="Reason"><TextInput value={reason} onChange={(e) => setReason(e.target.value)} maxLength={500} required /></Field>{error ? <Banner tone="error">{error}</Banner> : null}<AlertDialogFooter><Button tone="ghost" disabled={mutation.isPending} onClick={() => setOpen(false)}>Cancel</Button><Button tone="danger" isLoading={mutation.isPending} disabled={!reason.trim()} onClick={() => void retire()}>Retire version</Button></AlertDialogFooter></AlertDialogContent></AlertDialog>
  </Card>;
}

export function BusinessBundles() {
  const offers = useBundles(true);
  const canWrite = useCapability(useBusiness().data, "commercialWrite");
  const [editing, setEditing] = useState(false);
  return <div className="flex flex-col gap-4">
    <div className="flex items-center justify-between gap-4"><h2 className="text-xl font-bold">Credit bundles</h2>{canWrite && !editing ? <Button onClick={() => setEditing(true)} disabled={!offers.data}>Publish bundle</Button> : null}</div>
    {offers.isError ? <Banner tone="error">Bundle history could not be loaded. <Button tone="ghost" onClick={() => void offers.refetch()}>Retry</Button></Banner> : null}
    {offers.isLoading ? <Skeleton className="h-32 w-full" /> : null}
    {editing && offers.data ? <BundleEditor offers={offers.data} done={() => setEditing(false)} /> : null}
    {!offers.isLoading && offers.data?.length === 0 ? <Card title="No published bundles"><p>Publish a price and fixed purchased and promotional amounts to make a bundle available in customer checkout.</p></Card> : null}
    {offers.data?.map((offer) => <BundleCard key={offer.bundleVersionId} offer={offer} canWrite={canWrite} />)}
  </div>;
}
