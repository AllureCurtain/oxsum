import { useEffect, useRef, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { CheckCircle2, FileCheck2, ShieldAlert } from "lucide-react";
import { api } from "../api/client";
import { useSession, type SessionInfo } from "../api/hooks";
import { archiveKey, loadHeadVerifier, readArchive, type LedgerIdentity } from "../lib/retained-head";
import { downloadEvidence, evidenceBody, evidencePeriod, expectedEvidence, verifyEvidence, type EvidenceBundle, type EvidenceExpected, type EvidenceResult, type EvidenceSummary, type EvidenceVerifier } from "../lib/evidence";
import { Banner, Button, Card, CodeBlock, EmptyRows, Field, PageState, TableShell, Td, TextInput } from "../components/ui";

export function Evidence() {
  const session = useSession();
  const { t } = useTranslation();
  if (!session.data) return <PageState>{session.isError ? t("evidence.loadError") : t("common.loading")}</PageState>;
  if (session.data.role === "member") return <Banner tone="info">{t("evidence.roleRequired")}</Banner>;
  const org = session.data.organization;
  const identity = { site: window.location.origin, organizationId: org.id, tenantId: org.tenantId };
  return <CustomerEvidence key={archiveKey(identity)} identity={identity} />;
}

function utcDay(offset: number): string { return new Date(Date.now() - offset * 86_400_000).toISOString().slice(0, 10); }
function CustomerEvidence({ identity }: { identity: LedgerIdentity }) {
  const { t } = useTranslation();
  const client = useQueryClient();
  const [search] = useSearchParams();
  const statement = search.get("statement");
  const validStatement = statement !== null && /^[0-9a-f-]{36}$/i.test(statement);
  const queryKey = ["evidence-bundles", identity.organizationId];
  const list = useQuery({ queryKey, queryFn: () => api<{ rows: EvidenceSummary[] }>("/org/evidence-bundles") });
  const [from, setFrom] = useState(utcDay(30));
  const [to, setTo] = useState(utcDay(1));
  const [submitted, setSubmitted] = useState<{ from: string; to: string } | null>(null);
  const [selected, setSelected] = useState<{ bundle: EvidenceBundle; json: string; expected: EvidenceExpected } | null>(null);
  const [result, setResult] = useState<EvidenceResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const working = useRef(false);
  const active = useRef(true);
  useEffect(() => { active.current = true; return () => { active.current = false; }; }, []);
  function current() {
    const session = client.getQueryData<SessionInfo>(["session"]);
    return active.current && session?.organization.id === identity.organizationId && session.organization.tenantId === identity.tenantId;
  }
  async function run(action: () => Promise<void>, clearVerdict = true) {
    if (working.current) return;
    working.current = true; setBusy(true); setError(null); if (clearVerdict) setResult(null);
    try { await action(); }
    catch (failure) { if (current()) setError(failure instanceof Error ? failure.message : String(failure)); }
    finally { working.current = false; if (active.current) setBusy(false); }
  }
  async function fetchBundle(path: string) {
    const response = await fetch(`/api/v1${path}`, { credentials: "same-origin" });
    const text = await response.text();
    if (!response.ok) throw new Error(`${t("evidence.loadError")} (HTTP ${response.status})`);
    return evidenceBody(text);
  }
  async function load(summary: EvidenceSummary) {
    setSelected(null);
    const expected = expectedEvidence(identity, summary);
    const { bundle, json } = await fetchBundle(`/org/evidence-bundles/${summary.bundleId}`);
    if (!current()) return;
    if (bundle.organizationId !== expected.organizationId || bundle.bundleId !== expected.bundleId || bundle.manifestDigest !== expected.manifestDigest) throw new Error(t("evidence.selectionChanged"));
    setSelected({ bundle, json, expected });
  }
  async function mint() {
    const body = submitted ?? evidencePeriod(from, to);
    setSubmitted(body);
    const summary = await api<EvidenceSummary>("/org/evidence-bundles", { method: "POST", body });
    if (!current()) return;
    if (summary.kind !== "period" || summary.periodFrom !== body.from || summary.periodTo !== body.to) throw new Error(t("evidence.selectionChanged"));
    await client.invalidateQueries({ queryKey });
    await load(summary);
  }
  async function statementEvidence() {
    if (!validStatement) return;
    setSelected(null);
    const { bundle, json } = await fetchBundle(`/org/statements/${statement}/evidence`);
    if (!current()) return;
    if (bundle.organizationId !== identity.organizationId || bundle.statementId !== statement || bundle.kind !== "statement") throw new Error(t("evidence.selectionChanged"));
    setSelected({ bundle, json, expected: expectedEvidence(identity, bundle) });
    await client.invalidateQueries({ queryKey });
  }
  async function verify() {
    if (!selected) return;
    const retained = readArchive(localStorage.getItem(archiveKey(identity)), identity);
    const module = await loadHeadVerifier() as EvidenceVerifier;
    const verdict = await verifyEvidence(selected.json, selected.expected, identity, retained, module);
    if (current()) setResult(verdict);
  }
  return <div className="flex flex-col gap-6">
    <div><h1 className="text-2xl font-bold">{t("evidence.title")}</h1><p className="text-sm text-muted">{t("evidence.intro")}</p></div>
    <p className="flex flex-wrap gap-4 text-sm"><Link className="text-primary" to="/bills">{t("evidence.retainTrust")}</Link><a className="text-primary" href="https://github.com/AllureCurtain/oxsum/blob/main/docs/verify-your-bill.md">{t("bills.offlineGuide")}</a></p>
    {error ? <Banner tone="error"><ShieldAlert className="inline size-4" /> {error}</Banner> : null}
    {statement ? <Card title={t("evidence.statementTitle")}><p className="break-all font-mono text-sm">{statement}</p><p className="mt-2 text-sm">{t("evidence.statementNotice")}</p><Button className="mt-3" tone="ghost" disabled={busy || !validStatement} onClick={() => run(statementEvidence)}>{t("evidence.statementLoad")}</Button></Card> : null}
    <Card title={t("evidence.periodTitle")}>
      <p className="mb-3 text-sm">{t("evidence.mintNotice")}</p>
      <form className="flex flex-wrap items-end gap-3" onSubmit={(event) => { event.preventDefault(); void run(mint); }}>
        <Field label={t("evidence.from")}><TextInput type="date" required value={from} disabled={busy || submitted !== null} onChange={(event) => setFrom(event.target.value)} /></Field>
        <Field label={t("evidence.to")}><TextInput type="date" required max={utcDay(1)} value={to} disabled={busy || submitted !== null} onChange={(event) => setTo(event.target.value)} /></Field>
        <Button type="submit" disabled={busy} isLoading={busy}>{t(submitted ? "evidence.retrySpan" : "evidence.mint")}</Button>
        {submitted ? <Button type="button" tone="ghost" disabled={busy} onClick={() => { setSubmitted(null); setError(null); }}>{t("evidence.newSpan")}</Button> : null}
      </form>
      {submitted ? <p className="mt-2 font-mono text-sm">{submitted.from} — {submitted.to} UTC</p> : null}
    </Card>
    <Card title={t("evidence.saved")}>
      {list.isError ? <Banner tone="error">{t("evidence.loadError")} <Button tone="ghost" onClick={() => list.refetch()}>{t("common.retry")}</Button></Banner> : <TableShell loading={list.isLoading ? 4 : 0} head={[t("evidence.period"), t("evidence.digest"), t("evidence.entries"), ""]}>
        {(list.data?.rows ?? []).map((row) => <tr key={row.bundleId}><td>{row.kind === "period" ? `${row.periodFrom} — ${row.periodTo}` : row.statementId}<span className="block text-xs text-muted">{row.createdAt}</span></td><Td mono truncate>{row.manifestDigest}</Td><Td mono align="right">{row.entryCount}</Td><td><Button tone="ghost" disabled={busy} onClick={() => run(() => load(row))}>{t("evidence.open")}</Button></td></tr>)}
        {!list.isLoading && list.data?.rows.length === 0 ? <EmptyRows colSpan={4} icon={<FileCheck2 />}>{t("evidence.empty")}</EmptyRows> : null}
      </TableShell>}
    </Card>
    {selected ? <Card title={t("evidence.selected")}>
      <CodeBlock language="json" code={JSON.stringify({ expected: selected.expected, head: selected.bundle.head, scope: selected.bundle.scope }, null, 2)} />
      <p className="mt-3 text-sm">{t("evidence.scopeNote")}</p>
      <div className="mt-3 flex flex-wrap gap-3">
        <Button tone="ghost" disabled={busy} onClick={() => run(async () => downloadEvidence(selected.json, `oxsum-evidence-${selected.expected.bundleId}.json`), false)}>{t("evidence.download")}</Button>
        <Button tone="ghost" disabled={busy} onClick={() => run(async () => {
          const retained = readArchive(localStorage.getItem(archiveKey(identity)), identity);
          downloadEvidence({ expected: selected.expected, retained, suppliedKey: { keyName: selected.bundle.keyName, publicKey: selected.bundle.publicKey, keyHash: selected.bundle.keyHash }, verification: result }, `oxsum-evidence-inputs-${selected.expected.bundleId}.json`);
        }, false)}>{t("evidence.downloadInputs")}</Button>
        <Button disabled={busy} onClick={() => run(verify)}>{t("evidence.verify")}</Button>
      </div>
      <div className="mt-3" aria-live="polite">{result ? <Banner tone={result.signature === "retainedKey" ? "success" : "warn"}><CheckCircle2 className="inline size-4" /> {t(`evidence.verified_${result.signature}`)}</Banner> : <p className="text-sm text-muted">{t("evidence.notVerified")}</p>}</div>
    </Card> : null}
  </div>;
}
