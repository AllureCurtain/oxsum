import { useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
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
  const { t } = useTranslation();
  const terms = offer.terms;
  return <dl className="grid grid-cols-1 gap-4 text-sm sm:grid-cols-3">
    <div><dt className="text-muted">{t("biz.bundles.price")}</dt><dd className="mono text-right">{decimal(terms.displayAmountMinor, 2)} {terms.displayCurrency}</dd></div>
    <div><dt className="text-muted">{t("biz.bundles.purchased")}</dt><dd className="text-right"><Cr minor={terms.purchasedMinor} /></dd></div>
    <div><dt className="text-muted">{t("biz.bundles.promo")}</dt><dd className="text-right"><Cr minor={terms.promoMinor} /></dd></div>
    <div className="sm:col-span-3"><dt className="text-muted">{t("biz.bundles.validity")}</dt><dd>{terms.promoLifetimeDays == null ? t("biz.bundles.noExpiry") : t("biz.bundles.expiryDays", { days: terms.promoLifetimeDays })}. {t("biz.bundles.refundPolicy")}</dd></div>
  </dl>;
}

function BundleEditor({ offers, done }: { offers: BundleOffer[]; done: () => void }) {
  const { t } = useTranslation();
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
      setError(t("biz.bundles.invalidTerms"));
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
    catch (err) { setError(err instanceof Error ? err.message : t("biz.bundles.publishFailed")); }
  };
  return <Card title={t("biz.bundles.publishTitle")}>
    <form onSubmit={submit} className="flex flex-col gap-4">
      <Field label={t("biz.bundles.family")} hint={t("biz.bundles.familyHint")}>
        <NativeSelect value={base?.bundleId ?? ""} onChange={(e) => setBase(latest.get(e.target.value) ?? null)}>
          <option value="">{t("biz.bundles.newOffer")}</option>{[...latest.values()].map((offer) => <option key={offer.bundleId} value={offer.bundleId}>{offer.terms.name} · {t("biz.bundles.latestVersion", { version: offer.version })}</option>)}
        </NativeSelect>
      </Field>
      {base && latest.get(base.bundleId)?.version !== base.version ? <Banner tone="warn">{t("biz.bundles.newerPublished")}</Banner> : null}
      <Field label={t("biz.bundles.name")}><TextInput value={name} onChange={(e) => setName(e.target.value)} maxLength={200} required /></Field>
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
        <Field label={t("biz.bundles.priceField")}><TextInput inputMode="decimal" value={price} onChange={(e) => setPrice(e.target.value)} required /></Field>
        <Field label={t("biz.bundles.currency")} hint={t("biz.bundles.currencyHint")}><TextInput value={currency} onChange={(e) => setCurrency(e.target.value.toUpperCase())} pattern="[A-Z]{3}" maxLength={3} required /></Field>
        <Field label={t("biz.bundles.purchasedField")}><TextInput inputMode="decimal" value={purchased} onChange={(e) => setPurchased(e.target.value)} required /></Field>
        <Field label={t("biz.bundles.promoField")}><TextInput inputMode="decimal" value={promo} onChange={(e) => setPromo(e.target.value)} required /></Field>
        <Field label={t("biz.bundles.lifetimeField")} hint={t("biz.bundles.lifetimeHint")}><TextInput inputMode="numeric" value={lifetime} onChange={(e) => setLifetime(e.target.value)} /></Field>
      </div>
      <Field label={t("biz.reason")}><TextInput value={reason} onChange={(e) => setReason(e.target.value)} required maxLength={500} /></Field>
      {error ? <Banner tone="error">{error}</Banner> : null}
      <div className="flex gap-2"><Button type="submit" disabled={!name.trim() || !reason.trim()}>{t("biz.bundles.review")}</Button><Button type="button" tone="ghost" onClick={() => setDiscard(true)}>{t("biz.bundles.discard")}</Button></div>
    </form>
    <AlertDialog open={confirmation !== null} onOpenChange={(open) => { if (!open && !mutation.isPending) setConfirmation(null); }}>
      <AlertDialogContent><AlertDialogHeader><AlertDialogTitle>{t("biz.bundles.publishConfirm", { name: confirmation?.terms.name })}</AlertDialogTitle><AlertDialogDescription>{t("biz.bundles.publishExplain")}</AlertDialogDescription></AlertDialogHeader>
        {confirmation ? <Terms offer={{ bundleId: base?.bundleId ?? "", bundleVersionId: "", version: (base?.version ?? 0) + 1, terms: confirmation.terms, publishedAt: "", retiredAt: null }} /> : null}
        {error ? <Banner tone="error">{error}</Banner> : null}
        <AlertDialogFooter><Button tone="ghost" disabled={mutation.isPending} onClick={() => setConfirmation(null)}>{t("biz.bundles.keepEditing")}</Button><Button isLoading={mutation.isPending} onClick={() => void publish()}>{t("biz.bundles.publishVersion")}</Button></AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
    <AlertDialog open={discard} onOpenChange={setDiscard}><AlertDialogContent><AlertDialogHeader><AlertDialogTitle>{t("biz.bundles.discardTitle")}</AlertDialogTitle><AlertDialogDescription>{t("biz.bundles.discardExplain")}</AlertDialogDescription></AlertDialogHeader><AlertDialogFooter><Button tone="ghost" onClick={() => setDiscard(false)}>{t("biz.bundles.keepDraft")}</Button><Button tone="danger" onClick={done}>{t("biz.bundles.discardConfirm")}</Button></AlertDialogFooter></AlertDialogContent></AlertDialog>
  </Card>;
}

function BundleCard({ offer, canWrite }: { offer: BundleOffer; canWrite: boolean }) {
  const { t } = useTranslation();
  const mutation = useBundleMutation();
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const retire = async () => {
    try { await mutation.submit(`/business/bundles/${offer.bundleVersionId}/retire`, { reason: reason.trim() }); setOpen(false); }
    catch (err) { setError(err instanceof Error ? err.message : t("biz.bundles.retireFailed")); }
  };
  return <Card title={`${offer.terms.name} · ${t("biz.bundles.version", { version: offer.version })}`} actions={<div className="flex items-center gap-2"><Badge tone={offer.retiredAt ? "secondary" : "success"}>{offer.retiredAt ? t("biz.bundles.retired") : t("biz.bundles.onSale")}</Badge>{canWrite && !offer.retiredAt ? <Button tone="ghost" onClick={() => setOpen(true)}>{t("biz.bundles.retire")}</Button> : null}</div>}>
    <Terms offer={offer} /><p className="mt-4 text-xs text-muted">{t("biz.bundles.publishedAt", { at: offer.publishedAt })}{offer.retiredAt ? ` · ${t("biz.bundles.retiredAt", { at: offer.retiredAt })}` : ""}</p>
    <AlertDialog open={open} onOpenChange={(value) => { if (!mutation.isPending) setOpen(value); }}><AlertDialogContent><AlertDialogHeader><AlertDialogTitle>{t("biz.bundles.retireTitle", { name: offer.terms.name, version: offer.version })}</AlertDialogTitle><AlertDialogDescription>{t("biz.bundles.retireExplain")}</AlertDialogDescription></AlertDialogHeader><Field label={t("biz.reason")}><TextInput value={reason} onChange={(e) => setReason(e.target.value)} maxLength={500} required /></Field>{error ? <Banner tone="error">{error}</Banner> : null}<AlertDialogFooter><Button tone="ghost" disabled={mutation.isPending} onClick={() => setOpen(false)}>{t("common.cancel")}</Button><Button tone="danger" isLoading={mutation.isPending} disabled={!reason.trim()} onClick={() => void retire()}>{t("biz.bundles.retireVersion")}</Button></AlertDialogFooter></AlertDialogContent></AlertDialog>
  </Card>;
}

export function BusinessBundles() {
  const { t } = useTranslation();
  const offers = useBundles(true);
  const canWrite = useCapability(useBusiness().data, "commercialWrite");
  const [editing, setEditing] = useState(false);
  return <div className="flex flex-col gap-4">
    <div className="flex items-center justify-between gap-4"><h2 className="text-xl font-bold">{t("biz.bundles.title")}</h2>{canWrite && !editing ? <Button onClick={() => setEditing(true)} disabled={!offers.data}>{t("biz.bundles.publish")}</Button> : null}</div>
    {offers.isError ? <Banner tone="error">{t("biz.bundles.loadError")} <Button tone="ghost" onClick={() => void offers.refetch()}>{t("common.retry")}</Button></Banner> : null}
    {offers.isLoading ? <Skeleton className="h-32 w-full" /> : null}
    {editing && offers.data ? <BundleEditor offers={offers.data} done={() => setEditing(false)} /> : null}
    {!offers.isLoading && offers.data?.length === 0 ? <Card title={t("biz.bundles.emptyTitle")}><p>{t("biz.bundles.emptyNote")}</p></Card> : null}
    {offers.data?.map((offer) => <BundleCard key={offer.bundleVersionId} offer={offer} canWrite={canWrite} />)}
  </div>;
}
