import { useState } from "react";
import { useBusiness, useSaleBookHistory } from "../../api/hooks";
import { useSaleChange, useSaleDirectory, type SaleHistory } from "../../api/hooks/sales";
import { Banner, Button, Card, Field, NativeSelect, PageState, TextInput, TableShell, Td, Cr, AlertDialog, AlertDialogContent, AlertDialogTitle, AlertDialogDescription } from "../../components/ui";
import { modelDraft, modelsValue, type ModelDraft } from "../../lib/sale-draft";
import { SaleFields } from "./sale-fields";
import { SaleSchedules } from "./sale-schedules";
import { SalePreview } from "./sale-preview";

function DraftEditor({ history }: { history: SaleHistory }) {
  const latest = history.drafts.at(-1);
  const [revision, setRevision] = useState(latest?.revision ?? 0);
  const [rows, setRows] = useState<ModelDraft[]>(() => latest ? Object.entries(latest.models).map(([name, terms]) => modelDraft(name, terms)) : [modelDraft()]);
  const [reason, setReason] = useState("");
  const [error, setError] = useState("");
  const [saved, setSaved] = useState("");
  const [reload, setReload] = useState(false);
  const [removeIndex, setRemoveIndex] = useState<number | null>(null);
  const save = useSaleChange();
  return <Card title={`Draft editor · loaded revision ${revision}`}><div className="flex flex-col gap-4">
    <p>Customer sale prices are credits. One credit is 1,000,000 minor units. Supplier cost is configured separately. Saving creates a new draft revision and changes no active customer price.</p>
    {latest && latest.revision !== revision ? <Banner tone="warn">Revision {latest.revision} is now available. Your edits are retained; stale saves conflict.</Banner> : null}
    <form className="flex flex-col gap-4" onSubmit={async (event) => {
      event.preventDefault(); setError(""); setSaved("");
      try { const result = await save.submit({ operation: "saveDraft", bookId: history.book.id, expectedRevision: revision, models: modelsValue(rows) }, reason);
        if (result.revision == null) throw new Error("Save omitted its revision");
        setRevision(result.revision); setSaved(`Saved revision ${result.revision}. Publish an exact revision and then schedule it to activate.`);
      } catch (e) { setError(e instanceof Error ? e.message : "Save failed"); }
    }}>
      {rows.map((row, index) => <fieldset className="rounded-lg border border-line p-4" key={index}>
        <legend className="px-2">Model {index + 1}</legend><SaleFields value={row} onChange={(value) => setRows(rows.map((r, i) => i === index ? value : r))} />
        <Button type="button" tone="ghost" className="mt-4" onClick={() => setRemoveIndex(index)}>Remove model from draft</Button>
      </fieldset>)}
      <div className="flex flex-wrap gap-3"><Button type="button" tone="ghost" disabled={rows.length >= 500 || save.isPending} onClick={() => setRows([...rows, modelDraft()])}>Add model</Button>
        <Button type="button" tone="ghost" disabled={save.isPending || !latest} onClick={() => setReload(true)}>Load latest saved draft</Button></div>
      <Field label="Reason"><TextInput value={reason} maxLength={500} required onChange={(e) => setReason(e.target.value)} /></Field>
      {error ? <Banner tone="error">{error} Your inputs are retained.</Banner> : null}
      {saved ? <Banner tone="success">{saved}</Banner> : null}
      <Button type="submit" isLoading={save.isPending} disabled={!reason.trim()}>Save draft revision</Button>
    </form>
    <AlertDialog open={reload || removeIndex != null} onOpenChange={(open) => { if (!open) { setReload(false); setRemoveIndex(null); } }}>
      <AlertDialogContent><AlertDialogTitle>{reload ? "Replace unsaved editor content" : "Remove model from this draft"}</AlertDialogTitle>
        <AlertDialogDescription>{reload ? `Load saved revision ${latest?.revision} of ${history.book.name}. Unsaved editor content will be replaced.` : `Remove ${rows[removeIndex ?? 0]?.model || "the unnamed model"} from the editor. This takes effect only after saving, publishing and activating a new version.`}</AlertDialogDescription>
        <div className="flex gap-3"><Button type="button" tone="ghost" onClick={() => { setReload(false); setRemoveIndex(null); }}>Keep editing</Button>
          <Button type="button" tone="danger" onClick={() => {
            if (reload && latest) { setRows(Object.entries(latest.models).map(([name, terms]) => modelDraft(name, terms))); setRevision(latest.revision); setError(""); setSaved(""); save.reset(); }
            else if (removeIndex != null) setRows(rows.filter((_, i) => i !== removeIndex));
            setReload(false); setRemoveIndex(null);
          }}>Confirm</Button></div>
      </AlertDialogContent>
    </AlertDialog>
  </div></Card>;
}
function Publication({ history }: { history: SaleHistory }) {
  const [revision, setRevision] = useState(String(history.drafts.at(-1)?.revision ?? ""));
  const [reason, setReason] = useState("");
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const publish = useSaleChange();
  return <Card title="Publish an exact draft"><form className="flex flex-col gap-4" onSubmit={async (event) => {
    event.preventDefault(); setError(""); setMessage("");
    try { const draft = history.drafts.find((d) => String(d.revision) === revision); if (!draft) throw new Error("Select a saved draft");
      const result = await publish.submit({ operation: "publishDraft", bookId: history.book.id, draftRevision: draft.revision }, reason);
      setMessage(`Published immutable version ${result.revision} from draft ${draft.revision}. It is not active until scheduled or assigned.`);
    } catch (e) { setError(e instanceof Error ? e.message : "Publication failed"); }
  }}>
    <Field label="Saved draft revision"><NativeSelect value={revision} required onChange={(e) => setRevision(e.target.value)}><option value="">Select draft</option>{history.drafts.map((d) => <option key={d.revision} value={d.revision}>Revision {d.revision} · {Object.keys(d.models).length} models · {d.createdAt}</option>)}</NativeSelect></Field>
    <p>Published versions: {history.versions.map((v) => `v${v.version} (draft ${v.draftRevision})`).join(", ") || "none"}. Existing versions remain immutable.</p>
    <Field label="Reason"><TextInput value={reason} maxLength={500} required onChange={(e) => setReason(e.target.value)} /></Field>
    {error ? <Banner tone="error">{error}</Banner> : null}{message ? <Banner tone="success">{message}</Banner> : null}
    <Button type="submit" isLoading={publish.isPending} disabled={!revision || !reason.trim()}>Publish selected revision</Button>
  </form></Card>;
}
function SelectedBook({ id }: { id: string }) {
  const history = useSaleBookHistory(id);
  const principal = useBusiness();
  if (history.isLoading) return <PageState>Loading sale history…</PageState>;
  if (history.isError || !history.data) return <Banner tone="error">Could not load sale history. <Button tone="ghost" onClick={() => history.refetch()}>Retry</Button></Banner>;
  const h = history.data;
  try { for (const [name, terms] of Object.entries(h.drafts.at(-1)?.models ?? {})) modelDraft(name, terms); }
  catch (error) { return <Banner tone="error">{error instanceof Error ? error.message : "Stored amounts cannot be edited exactly"}. Use the exact-integer API; this editor will not round saved prices.</Banner>; }
  return <div className="flex flex-col gap-6"><h3 className="text-xl font-bold">{h.book.name}</h3>
    {principal.data?.capabilities.includes("catalogWrite") ? <><DraftEditor history={h} /><Publication history={h} /></> : <Banner tone="info">Your role can read sale terms and manage authorized customer assignments. Book editing requires catalog authority.</Banner>}
    <Card title="Published customer prices"><div className="flex flex-col gap-4">
      {!h.versions.length ? <p>No published versions.</p> : h.versions.map((v) => <details key={v.version}>
        <summary className="cursor-pointer text-primary">Version {v.version} · draft {v.draftRevision} · {v.publishedAt}</summary>
        <TableShell head={["Model", "Mode", { label: "Input credits / million", align: "right" }, { label: "Output credits / million", align: "right" }, "Discount", "Conditions"]}>
          {Object.entries(v.models).map(([name, terms]) => <tr key={name}><Td>{name}</Td><Td>{terms.price.mode ?? "chat"}</Td>
            <Td mono align="right"><Cr minor={terms.price.inputPricePerMillion} /></Td><Td mono align="right"><Cr minor={terms.price.outputPricePerMillion ?? 0} /></Td>
            <Td>{terms.discountPercent ?? 0}%</Td><Td>{terms.price.rules?.length ?? 0}</Td></tr>)}
        </TableShell><p className="text-sm">Use customer preview to inspect the selected condition and all charged dimensions.</p>
      </details>)}
    </div></Card>
    <SaleSchedules history={h} /><SalePreview history={h} />
  </div>;
}
export function BusinessPrices() {
  const principal = useBusiness();
  const books = useSaleDirectory(Boolean(principal.data && ["owner", "admin", "finance"].includes(principal.data.role)));
  const [selected, setSelected] = useState("");
  const [name, setName] = useState("");
  const [reason, setReason] = useState("");
  const [error, setError] = useState("");
  const create = useSaleChange();
  const role = principal.data?.role;
  if (role && !["owner", "admin", "finance"].includes(role)) return <Banner tone="info">Sale books require owner, admin or finance access.</Banner>;
  return <div className="flex flex-col gap-6"><h2 className="text-xl font-bold">Customer sale books</h2>
    {principal.data?.capabilities.includes("catalogWrite") ? <Card title="Create sale book"><form className="flex flex-col gap-4" onSubmit={async (e) => {
      e.preventDefault(); setError("");
      try { const result = await create.submit({ operation: "createBook", name }, reason); setSelected(result.resourceId); setName(""); setReason(""); }
      catch (e) { setError(e instanceof Error ? e.message : "Could not create book"); }
    }}><Field label="Book name"><TextInput value={name} maxLength={200} required onChange={(e) => setName(e.target.value)} /></Field>
      <Field label="Reason"><TextInput value={reason} maxLength={500} required onChange={(e) => setReason(e.target.value)} /></Field>
      {error ? <Banner tone="error">{error}</Banner> : null}<Button type="submit" isLoading={create.isPending} disabled={!name.trim() || !reason.trim()}>Create empty book</Button>
    </form></Card> : null}
    {books.isLoading ? <PageState>Loading books…</PageState> : books.isError ? <Banner tone="error">Sale directory could not be loaded. <Button tone="ghost" onClick={() => books.refetch()}>Retry</Button></Banner> : <Field label="Book"><NativeSelect value={selected} onChange={(e) => setSelected(e.target.value)}><option value="">Select a book</option>{books.data?.map((b) => <option key={b.id} value={b.id}>{b.name}</option>)}</NativeSelect></Field>}
    {!books.isLoading && !books.isError && !books.data?.length ? <PageState>No sale books yet. Create a book, save its model prices, publish, then activate a version.</PageState> : null}
    {selected ? <SelectedBook key={selected} id={selected} /> : null}
  </div>;
}
