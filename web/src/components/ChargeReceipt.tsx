import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "../api/client";
import type { components } from "../api/schema";
import { Banner, Button, Card, Field, Skeleton, TableShell, Td, TextInput } from "./ui";

type Receipt = components["schemas"]["ChargeReceipt"];
const credits = (n: number | null) => n == null ? "Not recorded" : `${(n / 1_000_000).toFixed(6)} credits`;

/** A deliberately allowlisted readable export, identical for staff and customers. */
export function receiptText(r: Receipt): string {
  return [
    "oxsum charge receipt", `Request: ${r.requestId}`, `Settlement: ${r.entryId ?? "Pending"}`,
    `Hash: ${r.contentHash ?? "Pending"}`, `Execution observation: ${r.executionState}`,
    `Financial outcome: ${r.financialState}; kind: ${r.settlementKind ?? "Not recorded"}; operation lifecycle: ${r.operationState ?? "Not recorded"}`, `Requested / served model: ${r.requestedModel} / ${r.servedModel ?? "Not recorded"}`,
    `Admission / terminal: ${r.createdAt ?? "Not recorded"} / ${r.completedAt ?? "Not recorded"}`,
    `Policy: ${r.policy}`, `Usage basis: ${r.usageBasis}`,
    `Price version: ${r.priceVersion ?? "Not recorded"}; sale version: ${r.saleVersion ?? "Not recorded"}; sale digest: ${r.saleDigest ?? "Not recorded"}`,
    `Ceiling: ${credits(r.ceilingMinor)}`, `Charge: ${credits(r.chargedMinor)}`,
    `Wallet pools: ${credits(r.walletChargedMinor)}; subscription: ${credits(r.subscriptionChargedMinor)}`,
    `Unused ceiling released: ${credits(r.releasedMinor)}`, `Discount: ${r.discountPercent ?? 0}%`, r.rounding,
    ...r.lines.map(l => `${l.item}: ${l.units} units × ${l.pricePerMillion} credit minor units / million`),
    "Ledger order (other operations can occur between these entries):",
    ...r.ledger.map(l => `#${l.index} ${l.bookedOn} ${l.entryId} ${l.pool}: reserved ${credits(l.reservedMinor)}, reservation release ${credits(l.releasedMinor)}, settled effect ${credits(l.settledEffectMinor)}`),
    "Attempt observations:", ...r.attempts.map(a => `${a.attemptId} ${a.externalId ?? "No provider ID"}: ${a.status}, ${a.startedAt} — ${a.completedAt ?? "Open"}`),
    ...r.diagnostics, "Verification checks recorded entry inclusion and supported arithmetic, not provider truth or service delivery. Retain the hash and independently trusted log head.",
  ].join("\n");
}

export function ChargeReceiptLookup({ customerId, business = false }: { customerId?: string; business?: boolean }) {
  const [identifier, setIdentifier] = useState("");
  const [active, setActive] = useState("");
  const endpoint = business ? "/business/charge-receipt" : customerId ? `/business/customers/${customerId}/charge-receipt` : "/charge-receipt";
  const query = useQuery({ queryKey: ["charge-receipt", endpoint, active], enabled: active !== "", retry: false,
    queryFn: async () => {
      if (business) return api<{ organizationId: string; customerName: string; receipt: Receipt }>(`${endpoint}?identifier=${encodeURIComponent(active)}`);
      return { organizationId: customerId, customerName: undefined, receipt: await api<Receipt>(`${endpoint}?identifier=${encodeURIComponent(active)}`) };
    } });
  const r = query.data?.receipt;
  const resolvedCustomer = query.data?.organizationId;
  const [proofError, setProofError] = useState("");
  async function exportProof() {
    if (!r?.entryId || !resolvedCustomer) return;
    try {
      setProofError("");
      const bundle = await api<Record<string, unknown>>(`/business/customers/${resolvedCustomer}/charge-receipt/proof?identifier=${encodeURIComponent(r.entryId)}`);
      const url = URL.createObjectURL(new Blob([JSON.stringify(bundle, null, 2)], { type: "application/json" }));
      const a = document.createElement("a"); a.href = url; a.download = `charge-proof-${r.requestId}.json`; a.click(); URL.revokeObjectURL(url);
    } catch (error) { setProofError(error instanceof Error ? error.message : "Proof export failed"); }
  }
  function download() {
    if (!r) return;
    const url = URL.createObjectURL(new Blob([receiptText(r)], { type: "text/plain;charset=utf-8" }));
    const a = document.createElement("a"); a.href = url; a.download = `charge-${r.requestId}.txt`; a.click(); URL.revokeObjectURL(url);
  }
  return <Card title="Charge receipt">
    <form className="flex flex-col gap-4 sm:flex-row sm:items-end" onSubmit={e => { e.preventDefault(); const next = identifier.trim(); if (next === active) void query.refetch(); else setActive(next); }}>
      <Field label="Exact request, operation, ledger entry or provider ID">
        <TextInput value={identifier} onChange={e => setIdentifier(e.target.value)} maxLength={256} required />
      </Field>
      <Button type="submit" isLoading={query.isFetching} disabled={!identifier.trim()}>Find charge</Button>
    </form>
    <p className="mt-4 text-sm text-ink-secondary">Reads the durable financial record, including when request analytics are missing. Amounts use 1 credit = 1,000,000 minor units.</p>
    {query.isFetching ? <Skeleton className="mt-4 h-24 w-full" /> : null}
    {query.error ? <Banner tone="error">{query.error.message}</Banner> : null}
    {r ? <div className="mt-4 flex flex-col gap-4">
      <div className="flex flex-wrap items-center gap-4"><Button onClick={download}>Export readable receipt</Button>
        {r.entryId && !resolvedCustomer ? <a className="text-primary" href={`/verify?entry=${r.entryId}`}>Verify recorded evidence</a> : null}
        {r.entryId && resolvedCustomer ? <><Button onClick={() => void exportProof()}>Export proof bundle</Button><a className="text-primary" href="/verify" target="_blank" rel="noreferrer">Open verifier</a></> : null}
      </div>
      {query.data?.customerName ? <p>Customer: {query.data.customerName} · <span className="font-mono">{resolvedCustomer}</span></p> : null}
      {resolvedCustomer && r.entryId ? <p className="text-sm">Paste the exported proof bundle into the verifier with the retained hash: <span className="break-all font-mono">{r.contentHash}</span>. The original evidence export is separate from the redacted readable receipt.</p> : null}
      {proofError ? <Banner tone="error">{proofError}</Banner> : null}
      <dl className="grid grid-cols-1 gap-4 sm:grid-cols-2">
        {[["Request", r.requestId], ["Execution observation", r.executionState], ["Operation lifecycle", r.operationState ?? "Not recorded"], ["Settlement kind", r.settlementKind ?? "Not recorded"], ["Financial outcome", r.financialState], ["Requested / served", `${r.requestedModel} / ${r.servedModel ?? "Not recorded"}`], ["Admission", r.createdAt ?? "Not recorded"], ["Terminal observation", r.completedAt ?? "Not recorded"], ["Ceiling", credits(r.ceilingMinor)], ["Total charge", credits(r.chargedMinor)], ["Wallet pool charge", credits(r.walletChargedMinor)], ["Subscription charge", credits(r.subscriptionChargedMinor)], ["Unused ceiling released", credits(r.releasedMinor)], ["Usage basis", r.usageBasis]].map(([label, value]) => <div key={label}><dt className="text-sm text-ink-secondary">{label}</dt><dd className="break-all font-mono">{value}</dd></div>)}
      </dl>
      <p>{r.policy}</p>
      <p className="break-all">Historical price version: {r.priceVersion ?? "Not recorded"}; sale version: {r.saleVersion ?? "Not recorded"}. Sale digest: {r.saleDigest ?? "Not recorded"}.</p>
      <TableShell head={["Dimension", { label: "Units", align: "right" }, { label: "Minor units / million", align: "right" }]}>
        {r.lines.map((l, i) => <tr key={i}><td>{l.item}</td><Td mono align="right">{l.units}</Td><Td mono align="right">{l.pricePerMillion}</Td></tr>)}
      </TableShell>
      <p>Recorded discount: {r.discountPercent ?? 0}%. {r.rounding}</p>
      <TableShell head={["Ledger position", "Posting day", "Pool", { label: "Reserved", align: "right" }, { label: "Reservation released", align: "right" }, { label: "Settled effect", align: "right" }]}>
        {r.ledger.map((l, i) => <tr key={i}><Td mono>#{l.index}</Td><td>{l.bookedOn}</td><td>{l.pool}</td><Td mono align="right">{credits(l.reservedMinor)}</Td><Td mono align="right">{credits(l.releasedMinor)}</Td><Td mono align="right">{credits(l.settledEffectMinor)}</Td></tr>)}
      </TableShell>
      {r.attempts.length > 0 ? <TableShell head={["Attempt", "Provider ID", "Observation", "Started", "Completed"]}>
        {r.attempts.map(a => <tr key={a.attemptId}><Td mono>{a.attemptId}</Td><Td mono>{a.externalId ?? "Not recorded"}</Td><td>{a.status}</td><td>{a.startedAt}</td><td>{a.completedAt ?? "Not recorded"}</td></tr>)}
      </TableShell> : null}
      <ul className="list-disc pl-4 text-sm text-ink-secondary">{r.diagnostics.map(d => <li key={d}>{d}</li>)}</ul>
      <p className="text-sm text-ink-secondary">Verification checks recorded entry inclusion and supported arithmetic. It does not establish provider usage truth or customer delivery. Retain independently trusted log heads.</p>
    </div> : null}
  </Card>;
}
