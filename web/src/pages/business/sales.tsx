import { useTranslation } from "react-i18next";
import { saleError, saleLabel } from "../../lib/sale-text";
import { useState } from "react";
import { useBusiness, useSaleBookHistory } from "../../api/hooks";
import { useSaleChange, useSaleDirectory, type SaleHistory } from "../../api/hooks/sales";
import { Banner, Button, Card, Field, NativeSelect, PageState, TextInput, TableShell, Td, Cr, AlertDialog, AlertDialogContent, AlertDialogTitle, AlertDialogDescription } from "../../components/ui";
import { modelDraft, modelsValue, type ModelDraft } from "../../lib/sale-draft";
import { SaleFields } from "./sale-fields";
import { SaleSchedules } from "./sale-schedules";
import { SalePreview } from "./sale-preview";

function DraftEditor({ history }: { history: SaleHistory }) {
  const { t } = useTranslation();
  const latest = history.drafts.at(-1);
  const [revision, setRevision] = useState(latest?.revision ?? 0);
  const [rows, setRows] = useState<ModelDraft[]>(() => latest ? Object.entries(latest.models).map(([name, terms]) => modelDraft(name, terms)) : [modelDraft()]);
  const [reason, setReason] = useState("");
  const [error, setError] = useState("");
  const [saved, setSaved] = useState("");
  const [reload, setReload] = useState(false);
  const [removeIndex, setRemoveIndex] = useState<number | null>(null);
  const save = useSaleChange();
  return <Card title={t("sale.draftTitle", { revision })}><div className="flex flex-col gap-4">
    <p>{t("sale.draftHint")}</p>
    {latest && latest.revision !== revision ? <Banner tone="warn">{t("sale.newRevision", { revision: latest.revision })}</Banner> : null}
    <form className="flex flex-col gap-4" onSubmit={async (event) => {
      event.preventDefault(); setError(""); setSaved("");
      try { const result = await save.submit({ operation: "saveDraft", bookId: history.book.id, expectedRevision: revision, models: modelsValue(rows) }, reason);
        if (result.revision == null) throw new Error(t("sale.missingRevision"));
        setRevision(result.revision); setSaved(t("sale.savedDraft", { revision: result.revision }));
      } catch (e) { setError(saleError(e, t, t("sale.saveError"))); }
    }}>
      {rows.map((row, index) => <fieldset className="rounded-lg border border-line p-4" key={index}>
        <legend className="px-2">{t("sale.modelNumber", { number: index + 1 })}</legend><SaleFields value={row} onChange={(value) => setRows(rows.map((r, i) => i === index ? value : r))} />
        <Button type="button" tone="ghost" className="mt-4" onClick={() => setRemoveIndex(index)}>{t("sale.removeModel")}</Button>
      </fieldset>)}
      <div className="flex flex-wrap gap-3"><Button type="button" tone="ghost" disabled={rows.length >= 500 || save.isPending} onClick={() => setRows([...rows, modelDraft()])}>{t("sale.addModel")}</Button>
        <Button type="button" tone="ghost" disabled={save.isPending || !latest} onClick={() => setReload(true)}>{t("sale.loadLatest")}</Button></div>
      <Field label={t("sale.reason")}><TextInput value={reason} maxLength={500} required onChange={(e) => setReason(e.target.value)} /></Field>
      {error ? <Banner tone="error">{error} {t("sale.inputsRetained")}</Banner> : null}
      {saved ? <Banner tone="success">{saved}</Banner> : null}
      <Button type="submit" isLoading={save.isPending} disabled={!reason.trim()}>{t("sale.saveDraft")}</Button>
    </form>
    <AlertDialog open={reload || removeIndex != null} onOpenChange={(open) => { if (!open) { setReload(false); setRemoveIndex(null); } }}>
      <AlertDialogContent><AlertDialogTitle>{reload ? t("sale.replaceUnsaved") : t("sale.removeTitle")}</AlertDialogTitle>
        <AlertDialogDescription>{reload ? t("sale.reloadDescription", { revision: latest?.revision, book: history.book.name }) : t("sale.removeDescription", { model: rows[removeIndex ?? 0]?.model || t("sale.unnamedModel") })}</AlertDialogDescription>
        <div className="flex gap-3"><Button type="button" tone="ghost" onClick={() => { setReload(false); setRemoveIndex(null); }}>{t("sale.keepEditing")}</Button>
          <Button type="button" tone="danger" onClick={() => {
            if (reload && latest) { setRows(Object.entries(latest.models).map(([name, terms]) => modelDraft(name, terms))); setRevision(latest.revision); setError(""); setSaved(""); save.reset(); }
            else if (removeIndex != null) setRows(rows.filter((_, i) => i !== removeIndex));
            setReload(false); setRemoveIndex(null);
          }}>{t("sale.confirm")}</Button></div>
      </AlertDialogContent>
    </AlertDialog>
  </div></Card>;
}
function Publication({ history }: { history: SaleHistory }) {
  const { t } = useTranslation();
  const [revision, setRevision] = useState(String(history.drafts.at(-1)?.revision ?? ""));
  const [reason, setReason] = useState("");
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const publish = useSaleChange();
  return <Card title={t("sale.publishTitle")}><form className="flex flex-col gap-4" onSubmit={async (event) => {
    event.preventDefault(); setError(""); setMessage("");
    try { const draft = history.drafts.find((d) => String(d.revision) === revision); if (!draft) throw new Error(t("sale.selectSavedDraft"));
      const result = await publish.submit({ operation: "publishDraft", bookId: history.book.id, draftRevision: draft.revision }, reason);
      setMessage(t("sale.publishedMessage", { version: result.revision, revision: draft.revision }));
    } catch (e) { setError(saleError(e, t, t("sale.publishError"))); }
  }}>
    <Field label={t("sale.savedRevision")}><NativeSelect value={revision} required onChange={(e) => setRevision(e.target.value)}><option value="">{t("sale.selectDraft")}</option>{history.drafts.map((d) => <option key={d.revision} value={d.revision}>{t("sale.draftOption", { revision: d.revision, count: Object.keys(d.models).length, at: d.createdAt })}</option>)}</NativeSelect></Field>
    <p>{t("sale.versions", { versions: history.versions.map((v) => t("sale.versionDraft", { version: v.version, revision: v.draftRevision })).join(", ") || t("sale.none") })}</p>
    <Field label={t("sale.reason")}><TextInput value={reason} maxLength={500} required onChange={(e) => setReason(e.target.value)} /></Field>
    {error ? <Banner tone="error">{error}</Banner> : null}{message ? <Banner tone="success">{message}</Banner> : null}
    <Button type="submit" isLoading={publish.isPending} disabled={!revision || !reason.trim()}>{t("sale.publish")}</Button>
  </form></Card>;
}
function SelectedBook({ id }: { id: string }) {
  const { t } = useTranslation();
  const history = useSaleBookHistory(id);
  const principal = useBusiness();
  if (history.isLoading) return <PageState>{t("sale.loadingHistory")}</PageState>;
  if (history.isError || !history.data) return <Banner tone="error">{t("sale.loadHistoryError")} <Button tone="ghost" onClick={() => history.refetch()}>{t("sale.retry")}</Button></Banner>;
  const h = history.data;
  try { for (const [name, terms] of Object.entries(h.drafts.at(-1)?.models ?? {})) modelDraft(name, terms); }
  catch (error) { return <Banner tone="error">{saleError(error, t, t("sale.storedError"))}. {t("sale.exactApi")}</Banner>; }
  return <div className="flex flex-col gap-6"><h3 className="text-xl font-bold">{h.book.name}</h3>
    {principal.data?.capabilities.includes("catalogWrite") ? <><DraftEditor history={h} /><Publication history={h} /></> : <Banner tone="info">{t("sale.readOnly")}</Banner>}
    <Card title={t("sale.publishedTitle")}><div className="flex flex-col gap-4">
      {!h.versions.length ? <p>{t("sale.noVersions")}</p> : h.versions.map((v) => <details key={v.version}>
        <summary className="cursor-pointer text-primary">{t("sale.versionSummary", { version: v.version, revision: v.draftRevision, at: v.publishedAt })}</summary>
        <TableShell head={[t("sale.model"), t("sale.mode"), { label: t("sale.inputMillion"), align: "right" }, { label: t("sale.outputMillion"), align: "right" }, t("sale.discount"), t("sale.conditions")]}>
          {Object.entries(v.models).map(([name, terms]) => <tr key={name}><Td>{name}</Td><Td>{saleLabel(terms.price.mode ?? "chat", t)}</Td>
            <Td mono align="right"><Cr minor={terms.price.inputPricePerMillion} /></Td><Td mono align="right"><Cr minor={terms.price.outputPricePerMillion ?? 0} /></Td>
            <Td>{terms.discountPercent ?? 0}%</Td><Td>{terms.price.rules?.length ?? 0}</Td></tr>)}
        </TableShell><p className="text-sm">{t("sale.previewHint")}</p>
      </details>)}
    </div></Card>
    <SaleSchedules history={h} /><SalePreview history={h} />
  </div>;
}
export function BusinessPrices() {
  const { t } = useTranslation();
  const principal = useBusiness();
  const books = useSaleDirectory(Boolean(principal.data && ["owner", "admin", "finance"].includes(principal.data.role)));
  const [selected, setSelected] = useState("");
  const [name, setName] = useState("");
  const [reason, setReason] = useState("");
  const [error, setError] = useState("");
  const create = useSaleChange();
  const role = principal.data?.role;
  if (role && !["owner", "admin", "finance"].includes(role)) return <Banner tone="info">{t("sale.roleRequired")}</Banner>;
  return <div className="flex flex-col gap-6"><h2 className="text-xl font-bold">{t("sale.title")}</h2>
    {principal.data?.capabilities.includes("catalogWrite") ? <Card title={t("sale.createTitle")}><form className="flex flex-col gap-4" onSubmit={async (e) => {
      e.preventDefault(); setError("");
      try { const result = await create.submit({ operation: "createBook", name }, reason); setSelected(result.resourceId); setName(""); setReason(""); }
      catch (e) { setError(saleError(e, t, t("sale.createError"))); }
    }}><Field label={t("sale.bookName")}><TextInput value={name} maxLength={200} required onChange={(e) => setName(e.target.value)} /></Field>
      <Field label={t("sale.reason")}><TextInput value={reason} maxLength={500} required onChange={(e) => setReason(e.target.value)} /></Field>
      {error ? <Banner tone="error">{error}</Banner> : null}<Button type="submit" isLoading={create.isPending} disabled={!name.trim() || !reason.trim()}>{t("sale.create")}</Button>
    </form></Card> : null}
    {books.isLoading ? <PageState>{t("sale.loadingBooks")}</PageState> : books.isError ? <Banner tone="error">{t("sale.loadDirectoryError")} <Button tone="ghost" onClick={() => books.refetch()}>{t("sale.retry")}</Button></Banner> : <Field label={t("sale.book")}><NativeSelect value={selected} onChange={(e) => setSelected(e.target.value)}><option value="">{t("sale.selectBook")}</option>{books.data?.map((b) => <option key={b.id} value={b.id}>{b.name}</option>)}</NativeSelect></Field>}
    {!books.isLoading && !books.isError && !books.data?.length ? <PageState>{t("sale.noBooks")}</PageState> : null}
    {selected ? <SelectedBook key={selected} id={selected} /> : null}
  </div>;
}
