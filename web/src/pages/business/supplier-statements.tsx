import { useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../../api/client";
import { useBusiness } from "../../api/hooks";
import type { components } from "../../api/schema";
import { Banner, Button, Card, Field, NativeSelect, TextInput, Textarea } from "../../components/ui";

type Input = components["schemas"]["SupplierStatementImport"];
type Preview = components["schemas"]["SupplierStatementPreview"];
type Detail = components["schemas"]["SupplierStatementDetail"];
type Summary = components["schemas"]["SupplierStatementSummary"];
type Comparison = components["schemas"]["SupplierComparison"];
type History = components["schemas"]["SupplierComparisonSummary"];
type Intent = { kind: "import"; body: Input } | { kind: "compare"; source: string; body: { idempotencyKey: string; reason: string } };
const base = "/business/supplier-statements";

export function SupplierStatements() {
  const business = useBusiness().data;
  if (!business) return null;
  return <StatementPanel key={`${business.businessId}:${business.userId}`} storageKey={`supplier-statement:${business.businessId}:${business.userId}`} canWrite={business.capabilities.includes("fundsWrite")} />;
}

function StatementPanel({ storageKey, canWrite }: { storageKey: string; canWrite: boolean }) {
  const client = useQueryClient();
  const [org, setOrg] = useState("");
  const [day, setDay] = useState("");
  const [fetched, setFetched] = useState("");
  const [projects, setProjects] = useState("");
  const [scope, setScope] = useState("organization");
  const [conversion, setConversion] = useState("");
  const [mapping, setMapping] = useState("");
  const [reason, setReason] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const [pages, setPages] = useState<{ name: string; body: string; cursor: string }[]>([]);
  const [reviewed, setReviewed] = useState<{ body: Input; preview: Preview }>();
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);
  const fileRevision=useRef(0);
  const [readingFiles,setReadingFiles]=useState(false);
  const [intent, setIntent] = useState<Intent | null>(() => { try { return JSON.parse(sessionStorage.getItem(storageKey) ?? "null") as Intent | null; } catch { return null; } });
  const [before, setBefore] = useState("");
  const [sourceId, setSourceId] = useState("");
  const [comparisonId, setComparisonId] = useState("");
  const [historyBefore, setHistoryBefore] = useState("");
  const [compareReason, setCompareReason] = useState("");
  const choices = useQuery({ queryKey: ["supplier-choices"], queryFn: () => api<components["schemas"]["SupplierChannelChoice"][]>(`${base}/choices`) });
  const sources = useQuery({ queryKey: ["supplier-sources", before], queryFn: () => api<Summary[]>(`${base}${before ? `?before=${before}` : ""}`) });
  const detail = useQuery({ queryKey: ["supplier-source", sourceId, comparisonId], enabled: !!sourceId, queryFn: () => api<Detail>(`${base}/${sourceId}${comparisonId ? `?comparisonId=${comparisonId}` : ""}`) });
  const history = useQuery({ queryKey: ["supplier-comparisons", sourceId, historyBefore], enabled: !!sourceId, queryFn: () => api<History[]>(`${base}/${sourceId}/comparisons${historyBefore ? `?before=${historyBefore}` : ""}`) });
  const resetPreview = () => setReviewed(undefined);
  const selectSource = (id: string) => { setSourceId(id); setComparisonId(""); setHistoryBefore(""); };
  async function preview() {
    if (inFlight.current) return;
    inFlight.current = true; setBusy(true); setError("");
    try {
      const start = Date.parse(`${day}T00:00:00Z`) / 1000;
      if (!Number.isSafeInteger(start)) throw new Error("Choose a complete UTC day.");
      const projectIds = scope === "projects" ? [...new Set(projects.split(/[\s,]+/).filter(Boolean))].sort() : [];
      const body: Input = { idempotencyKey: crypto.randomUUID(), reason, providerOrganizationId: org, billDate: day, fetchedAt: fetched, scopeKind: scope as Input["scopeKind"], projectIds, channelIds: selected, usdPerCredit: conversion, mappingEvidence: mapping, query: { startTime: start, endTime: start + 86400, bucketWidth: "1d", groupBy: scope === "projects" ? ["project_id"] : [], projectIds, limit: 7 }, pages: pages.map((p, i) => ({ requestPage: i === 0 ? null : p.cursor, body: p.body })) };
      setReviewed({ body, preview: await api<Preview>(`${base}/preview`, { method: "POST", body }) });
    } catch (e) { setError(e instanceof Error ? e.message : "Preview failed"); } finally { inFlight.current = false; setBusy(false); }
  }
  async function submit(next: Intent) {
    if (inFlight.current) return;
    inFlight.current = true; setBusy(true); setError("");
    try {
      // Persist before transport; failures retain exactly the same source/key.
      sessionStorage.setItem(storageKey, JSON.stringify(next)); setIntent(next);
      if (next.kind === "import") { const result = await api<Detail>(base, { method: "POST", body: next.body }); selectSource(result.sourceId); }
      else { const result = await api<Comparison>(`${base}/${next.source}/compare`, { method: "POST", body: next.body }); setSourceId(next.source); setComparisonId(result.comparisonId); }
      sessionStorage.removeItem(storageKey); setIntent(null); setReviewed(undefined);
      await client.invalidateQueries({ queryKey: ["supplier-sources"] }); await client.invalidateQueries({ queryKey: ["supplier-comparisons"] }); await client.invalidateQueries({ queryKey: ["supplier-source"] });
    } catch (e) { setError(e instanceof Error ? e.message : "Import failed"); } finally { inFlight.current = false; setBusy(false); }
  }
  function download() { if (!detail.data) return; const url = URL.createObjectURL(new Blob([JSON.stringify(detail.data, null, 2)], { type: "application/json" })); const a = document.createElement("a"); a.href = url; a.download = `openai-costs-${detail.data.sourceId}-${detail.data.comparison.comparisonId}.json`; a.click(); URL.revokeObjectURL(url); }
  const d = detail.data; const c = d?.comparison.document;
  return <Card title="OpenAI supplier statements">
    <p className="mb-4 text-sm text-ink-secondary">Import a complete UTC day from OpenAI Costs. This compares daily aggregates and preserves unknown cost; it does not establish request matches or change customer balances. Files do not prove organization identity. Retain the provider export and mapping evidence.</p>
    {error || choices.error || sources.error || detail.error || history.error ? <Banner tone="error">{error || choices.error?.message || sources.error?.message || detail.error?.message || history.error?.message}</Banner> : null}
    {choices.isLoading || sources.isLoading || detail.isFetching || history.isFetching ? <p>Loading retained supplier evidence…</p> : null}
    {history.error ? <Button tone="ghost" onClick={() => void history.refetch()}>Reload comparison history</Button> : null}
    {intent ? <Banner tone="warn"><p>An unresolved {intent.kind} is retained with its original bytes and retry identity.</p><Button disabled={!canWrite || busy} onClick={() => void submit(intent)}>Retry exact saved request</Button><p>Discarding the saved request does not undo a committed import. After a connection failure, check history or retry the exact request first.</p><Button disabled={busy} tone="ghost" onClick={() => { sessionStorage.removeItem(storageKey); setIntent(null); setReviewed(undefined); }}>Discard saved request and revise</Button></Banner> : null}
    {canWrite && !intent ? <fieldset disabled={busy || readingFiles} className="flex flex-col gap-4" onChange={resetPreview}>
      <div className="grid gap-4 sm:grid-cols-2">
        <Field label="OpenAI organization ID"><TextInput value={org} onChange={e => setOrg(e.target.value)} maxLength={200} /></Field>
        <Field label="UTC billing day"><TextInput type="date" value={day} onChange={e => setDay(e.target.value)} /></Field>
        <Field label="Original fetched time (RFC3339)"><TextInput value={fetched} onChange={e => setFetched(e.target.value)} placeholder="2026-11-02T12:00:00Z" /></Field>
        <Field label="Scope"><NativeSelect value={scope} onChange={e => setScope(e.target.value)}><option value="organization">Whole organization, ungrouped</option><option value="projects">Selected projects, grouped by project_id</option></NativeSelect></Field>
        {scope === "projects" ? <Field label="Project IDs"><TextInput value={projects} onChange={e => setProjects(e.target.value)} placeholder="proj_a, proj_b" /></Field> : null}
        <Field label="USD per local supplier-cost credit"><TextInput value={conversion} onChange={e => setConversion(e.target.value)} placeholder="Explicit conversion, e.g. 1" /></Field>
      </div>
      <fieldset><legend className="mb-2 text-sm font-medium">Channels covering this entire provider scope and day</legend>{choices.data?.map(choice => <label key={choice.channelId} className="mr-4 inline-flex items-center gap-2"><input type="checkbox" checked={selected.includes(choice.channelId)} onChange={e => setSelected(e.target.checked ? [...selected, choice.channelId] : selected.filter(id => id !== choice.channelId))} />{choice.name}</label>)}{choices.data?.length === 0 ? <p>No canonical OpenAI channels. Configure an OpenAI channel before importing.</p> : null}</fieldset>
      <Field label="Mapping and conversion evidence"><Textarea value={mapping} onChange={e => setMapping(e.target.value)} maxLength={2000} placeholder="Explain the organization/project → channel mapping and supplier-cost unit conversion for this UTC day." /></Field>
      <Field label="Audit reason"><TextInput value={reason} onChange={e => setReason(e.target.value)} maxLength={500} /></Field>
      <Field label="Raw JSON response pages, in fetch order"><input type="file" accept=".json,application/json" multiple onChange={async e => { resetPreview(); const revision=++fileRevision.current; const files = Array.from(e.target.files ?? []); setPages([]); if (files.length > 16 || files.reduce((n, f) => n + f.size, 0) > 2 * 1024 * 1024) { setError("Maximum 16 pages and 2 MiB."); return; } setReadingFiles(true); try { const read=await Promise.all(files.map(async f => ({ name: f.name, body: await f.text(), cursor: "" }))); if(fileRevision.current===revision) setPages(read); } catch(e) {setError(e instanceof Error ? e.message : "File read failed");} finally {if(fileRevision.current===revision) setReadingFiles(false);} }} /></Field>
      <p className="text-sm text-ink-secondary">Export query: bucket_width=1d, limit=7, the selected UTC start/end; only project_id grouping and selected project_ids are supported. Upload original response text without editing numeric values. Additional filters are unsupported.</p>
      {pages.map((p, i) => <div key={i}><p>{i + 1}. {p.name}</p>{i > 0 ? <Field label={`Request page cursor for page ${i + 1}`}><TextInput value={p.cursor} onChange={e => setPages(pages.map((row, at) => at === i ? { ...row, cursor: e.target.value } : row))} /></Field> : null}</div>)}
      <Button disabled={busy || !pages.length || !selected.length} onClick={() => void preview()}>Preview scope and amount</Button>
      {reviewed ? <div className="rounded border border-border p-4"><p className="font-semibold">Provider observation: USD {reviewed.preview.amountUsd}</p><p>{reviewed.preview.billDate} · {reviewed.preview.scopeKind} · {reviewed.preview.providerOrganizationId}</p><p>Reviewed channels: {reviewed.body.channelIds.map(id => choices.data?.find(c => c.channelId === id)?.name ?? id).join(", ")}</p><p>Reviewed USD per credit: {reviewed.body.usdPerCredit}</p><p>Reviewed mapping evidence: {reviewed.body.mappingEvidence}</p><p>Sub-microUSD residual: {reviewed.preview.residualUsd}</p><p>{reviewed.preview.identityBasis}</p><p className="my-2">Confirm that the uploaded pages and declared scope, channel mapping and cost conversion are accurate for this complete UTC day.</p><Button disabled={busy} onClick={() => void submit({ kind: "import", body: reviewed.body })}>Confirm import and compare</Button></div> : null}
    </fieldset> : null}
    <h3 className="mt-6 font-semibold">Immutable source history</h3>
    {sources.data?.length === 0 ? <p>No statements imported.</p> : null}
    <div className="my-3 flex flex-col gap-2">{sources.data?.map(s => <Button key={s.sourceId} tone="ghost" className="h-auto justify-start whitespace-normal text-left" onClick={() => selectSource(s.sourceId)}>{s.parsed.billDate} · {s.parsed.providerOrganizationId} · version {s.sourceVersion} · USD {s.parsed.amountUsd}</Button>)}</div>
    <div className="flex gap-2">{before ? <Button onClick={() => setBefore("")}>Newest sources</Button> : null}{sources.data?.length === 50 ? <Button onClick={() => setBefore(sources.data![49].sourceId)}>Older sources</Button> : null}</div>
    {d && c ? <div className="mt-6 flex flex-col gap-3 rounded border border-border p-4">
      <h3 className="font-semibold">Source version {d.sourceVersion} · {d.parsed.billDate}</h3><p className="break-all">Source: {d.sourceId} · imported by {d.importedBy}</p><p className="break-all">Digest: {d.digest}</p><p>Scope: {d.parsed.scopeKind} · {d.parsed.projectIds.join(", ") || "Whole organization"}</p><p>Mapping evidence: {d.source.mappingEvidence}</p>
      <h4 className="font-semibold">Frozen comparison</h4><p className="break-all">Comparison: {d.comparison.comparisonId}</p><p>Evidence as of transaction start: {c.capturedAt}</p><p>Provider observation: USD {c.providerAmountUsd}</p><p>Known local estimate: USD {c.knownLocalCostUsd}</p><p>Difference against known estimate: USD {c.deltaAgainstKnownUsd}</p><p>Exact difference residual: {c.deltaResidualNumerator} / {c.conversionRemainderDenominator} USD</p><p>Amount relation: {c.amountRelation.replaceAll("_", " ")}. Coverage remains incomplete.</p>
      <p>Attempts with cost: {c.coverage.knownAttempts}; unknown: {c.coverage.unknownAttempts}; additional attempts in this cohort: {c.coverage.retryAttempts}; pending: {c.coverage.pendingAttempts}; failed/cancelled: {c.coverage.failedOrCancelledAttempts}.</p><p>Diagnostic missing attempts: {c.coverage.diagnosticUnrecordedAttempts}; operations without attempts: {c.coverage.operationsWithoutAttempts}; cache rows: {c.coverage.diagnosticCacheRows}.</p><p>{c.coverage.scopeBasis}</p><ul className="list-disc pl-5">{c.coverage.limitations.map(l => <li key={l}>{l}</li>)}</ul>
      <Button onClick={download}>Export exact source and comparison</Button>
      <Field label="Comparison history"><NativeSelect value={comparisonId || d.comparison.comparisonId} onChange={e => setComparisonId(e.target.value)}>{!history.data?.some(h => h.comparisonId===d.comparison.comparisonId) ? <option value={d.comparison.comparisonId}>{c.capturedAt} · Selected retained comparison</option> : null}{history.data?.map(h => <option key={h.comparisonId} value={h.comparisonId}>{h.capturedAt} · {h.reason}</option>)}</NativeSelect></Field>{history.data?.length === 50 ? <Button onClick={() => setHistoryBefore(history.data![49].comparisonId)}>Older comparisons</Button> : null}{historyBefore ? <Button onClick={() => setHistoryBefore("")}>Latest comparisons</Button> : null}
      {canWrite && !intent ? <><Field label="Reason for a new comparison"><TextInput value={compareReason} onChange={e => setCompareReason(e.target.value)} maxLength={500} /></Field><Button disabled={busy || !compareReason.trim()} onClick={() => void submit({ kind: "compare", source: d.sourceId, body: { idempotencyKey: crypto.randomUUID(), reason: compareReason } })}>Capture another comparison</Button></> : null}
    </div> : null}
  </Card>;
}
