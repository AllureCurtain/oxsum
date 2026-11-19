import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "../../api/client";
import type { components } from "../../api/schema";
import { Banner, Button, Card, Cr, Field, NativeSelect } from "../../components/ui";

type Row = components["schemas"]["UsageRecoveryQueueRow"];

/** Durable pending finance and retained unknown supplier exposure. */
export function UsageRecoveryQueue() {
  const [closed, setClosed] = useState(false);
  const [before, setBefore] = useState("");
  const query = useQuery({ queryKey: ["usage-recovery-queue", closed, before], refetchInterval: 15_000,
    queryFn: () => api<Row[]>(`/business/usage-recovery?closedUnknown=${closed}${before ? `&before=${before}` : ""}`) });
  return <Card title="Usage recovery">
    <p className="mb-4 text-sm text-ink-secondary">Pending requests keep their reservation until evidence is priced or unresolved components are waived. Unknown supplier cost is exposure, not zero consumption. Closed requests never receive an automatic additional debit.</p>
    <Field label="Financial state"><NativeSelect value={closed ? "closed" : "pending"} onChange={e => { setClosed(e.target.value === "closed"); setBefore(""); }}><option value="pending">Executing or awaiting evidence</option><option value="closed">Closed with unknown supplier cost</option></NativeSelect></Field>
    {query.error ? <Banner tone="error">{query.error.message}</Banner> : null}
    {query.isLoading ? <p>Loading recovery records…</p> : null}
    <div className="mt-4 flex flex-col gap-4">{query.data?.map(row => <div key={row.operationId} className="rounded border border-border p-4">
      <p className="font-medium">{row.model} · {row.recovery.state}</p>
      <p className="break-all text-sm">Operation: {row.operationId}</p><p className="break-all text-sm">Customer: {row.organizationId}</p>
      <p>Customer ceiling: <Cr minor={row.ceilingMinor} /></p>
      <p>Execution observation: {row.recovery.terminalObservation ?? "Still executing"}</p>
      <p>Evidence deadline: {row.recovery.deadline ?? "Begins after terminal observation"}</p>
      <p>Provider input / output: {row.recovery.observed.input ?? "Unknown"} / {row.recovery.observed.output ?? "Unknown"}</p>
      {row.recovery.closeReason ? <p>Closure: {row.recovery.closeReason.replaceAll("_", " ")}</p> : null}
      {row.recovery.waived.length ? <p>Waived: {row.recovery.waived.map(value => value.replaceAll("_", " ")).join(", ")}</p> : null}
      <p className="text-sm text-ink-secondary">Staff with customer access can use this operation ID in Customers → Charge receipt for the ledger and readable export.</p>
    </div>)}</div>
    {query.data?.length === 0 ? <p className="mt-4">No recovery records in this view.</p> : null}
    <div className="mt-4 flex gap-2">{before ? <Button onClick={() => setBefore("")}>First page</Button> : null}{query.data?.length === 50 ? <Button onClick={() => setBefore(query.data![49].operationId)}>Next page</Button> : null}</div>
  </Card>;
}
