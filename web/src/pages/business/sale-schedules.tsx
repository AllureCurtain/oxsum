import { useEffect, useState } from "react";
import { useQueries } from "@tanstack/react-query";
import { api } from "../../api/client";
import { useBusiness, useBusinessCustomers } from "../../api/hooks";
import { useCustomerSaleTerms, useSaleChange, useSaleDirectory, type SaleChange, type SaleHistory, type SaleSchedule } from "../../api/hooks/sales";
import { Banner, Button, Card, Field, NativeSelect, TableShell, Td, TextInput, EmptyRows, AlertDialog, AlertDialogContent, AlertDialogTitle, AlertDialogDescription } from "../../components/ui";
import { modelDraft, scheduleState, termsValue, utc, utcMicros } from "../../lib/sale-draft";
import { SaleFields } from "./sale-fields";

export function CustomerPicker({ value, onChange }: { value: string; onChange: (id: string) => void }) {
  const [cursor, setCursor] = useState<string>();
  const [label, setLabel] = useState("");
  const customers = useBusinessCustomers(cursor);
  return <div className="flex flex-col gap-3">
    <Field label="Customer">
      <NativeSelect value={value} disabled={customers.isLoading} onChange={(e) => {
        onChange(e.target.value); setLabel(customers.data?.customers.find((c) => c.organizationId === e.target.value)?.name ?? "");
      }}>
        <option value="">Select a customer</option>
        {value && !customers.data?.customers.some((c) => c.organizationId === value) ? <option value={value}>{label || value}</option> : null}
        {customers.data?.customers.map((c) => <option key={c.organizationId} value={c.organizationId}>{c.name} · {c.externalId} · {c.state}</option>)}
      </NativeSelect>
    </Field>
    {customers.isError ? <Banner tone="error">Customers could not be loaded.</Banner> : null}
    <div className="flex gap-3"><Button type="button" tone="ghost" disabled={!cursor || customers.isFetching} onClick={() => setCursor(undefined)}>First customers</Button>
      <Button type="button" tone="ghost" disabled={!customers.data?.nextCursor || customers.isFetching} onClick={() => setCursor(customers.data?.nextCursor ?? undefined)}>More customers</Button></div>
  </div>;
}

export function ScheduleRows({ rows }: { rows: SaleSchedule[] }) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => { const timer = setInterval(() => setNow(Date.now()), 1000); return () => clearInterval(timer); }, []);
  const principal = useBusiness();
  const change = useSaleChange();
  const [ending, setEnding] = useState<SaleSchedule | null>(null);
  const [until, setUntil] = useState("");
  const [reason, setReason] = useState("");
  const [error, setError] = useState("");
  return <>
    <TableShell head={["State", "Scope", "Version / model", "From (UTC)", "Until (UTC)", "Action"]}>
      {rows.map((row) => <tr key={row.id}>
        <Td>{scheduleState(row, now)}</Td><Td>{row.kind}{row.organizationId ? ` · ${row.organizationId}` : ""}</Td>
        <Td mono>{row.model ?? `v${row.version}`}<span className="block text-xs">{row.bookId}</span></Td>
        <Td mono>{row.effectiveFrom}</Td><Td mono>{row.effectiveUntil ?? "Open ended"}</Td>
        <Td>{principal.data?.capabilities.includes(row.kind === "default" ? "catalogWrite" : "commercialWrite") && ["Active", "Scheduled"].includes(scheduleState(row, now)) ? <Button type="button" tone="ghost" onClick={() => {
          setEnding(row); setUntil(scheduleState(row) === "Scheduled" ? row.effectiveFrom : new Date(Date.now() + 60_000).toISOString()); setReason(""); setError(""); change.reset();
        }}>End / cancel</Button> : "—"}</Td>
      </tr>)}
      {!rows.length ? <EmptyRows colSpan={6}>No schedules recorded.</EmptyRows> : null}
    </TableShell>
    <AlertDialog open={ending != null} onOpenChange={(open) => { if (!open && !change.isPending) setEnding(null); }}>
      <AlertDialogContent><AlertDialogTitle>End sale schedule</AlertDialogTitle>
        <AlertDialogDescription>End {ending?.kind} {ending?.id}. New requests after the end resolve the remaining terms. Already admitted requests keep their original prices. Ending a future window at its start cancels it.</AlertDialogDescription>
        <Field label="End (UTC)"><TextInput value={until} onChange={(e) => setUntil(e.target.value)} /></Field>
        <Field label="Reason"><TextInput value={reason} maxLength={500} onChange={(e) => setReason(e.target.value)} /></Field>
        {error ? <Banner tone="error">{error}</Banner> : null}
        <div className="flex gap-3"><Button type="button" tone="ghost" disabled={change.isPending} onClick={() => setEnding(null)}>Keep schedule</Button>
          <Button type="button" tone="danger" isLoading={change.isPending} disabled={!reason.trim()} onClick={async () => {
            try { const end = utc(until); if (!end || !ending) throw new Error("An exact UTC end is required");
              await change.submit({ operation: "endSchedule", scheduleId: ending.id, effectiveUntil: end }, reason); setEnding(null);
            } catch (e) { setError(e instanceof Error ? e.message : "Could not end schedule"); }
          }}>Confirm end</Button></div>
      </AlertDialogContent>
    </AlertDialog>
  </>;
}

export function SaleSchedules({ history }: { history: SaleHistory }) {
  const principal = useBusiness();
  const catalog = principal.data?.capabilities.includes("catalogWrite");
  const commercial = principal.data?.capabilities.includes("commercialWrite");
  const directory = useSaleDirectory();
  const histories = useQueries({ queries: (directory.data ?? []).map((b) => ({ queryKey: ["biz-sale-book", b.id], queryFn: () => api<SaleHistory>(`/business/sale-books/${b.id}`) })) });
  const defaults = histories.flatMap((q) => q.data?.schedules.filter((s) => s.kind === "default") ?? []);
  const [kind, setKind] = useState<"default" | "assigned" | "override">(catalog ? "default" : "assigned");
  const [customer, setCustomer] = useState("");
  const customerTerms = useCustomerSaleTerms(customer);
  const [version, setVersion] = useState(String(history.versions.at(-1)?.version ?? ""));
  const [from, setFrom] = useState("");
  const [until, setUntil] = useState("");
  const [replaces, setReplaces] = useState("");
  const [reason, setReason] = useState("");
  const [override, setOverride] = useState(modelDraft());
  const mutation = useSaleChange();
  const [error, setError] = useState("");
  const [saved, setSaved] = useState("");
  const existing = kind === "default" ? defaults : (customerTerms.data ?? []).filter((s) => s.kind === kind && (kind !== "override" || s.model === override.model.trim()));
  const unavailable = kind === "default" ? directory.isLoading || directory.isError || histories.some((q) => q.isLoading || q.isError) : !customer || customerTerms.isLoading || customerTerms.isError;
  return <Card title="Activation and customer terms"><div className="flex flex-col gap-4">
    <p>Publishing alone never activates a price. Effective windows are UTC and exclude their end. Resolution uses a customer model override, then its assigned book, then the business default. Missing models fall through; an explicit zero price wins.</p>
    {(catalog || commercial) ? <form className="flex flex-col gap-4" onSubmit={async (event) => {
      event.preventDefault(); setError(""); setSaved("");
      try {
        const effectiveFrom = utc(from), effectiveUntil = utc(until);
        if (effectiveFrom && effectiveUntil && utcMicros(effectiveFrom) >= utcMicros(effectiveUntil)) throw new Error("End must follow the start");
        const window = { effectiveFrom, effectiveUntil, replaces: replaces || null };
        let change: SaleChange;
        if (kind === "override") change = { operation: "overrideModel", organizationId: customer, model: override.model.trim(), terms: termsValue(override), ...window };
        else { const selected = history.versions.find((v) => String(v.version) === version); if (!selected) throw new Error("Select an immutable published version");
          change = kind === "default" ? { operation: "scheduleDefault", bookId: history.book.id, version: selected.version, ...window } : { operation: "assignBook", organizationId: customer, bookId: history.book.id, version: selected.version, ...window }; }
        const result = await mutation.submit(change, reason); setSaved(`Recorded schedule ${result.resourceId}. Preview a customer to see the effective result.`);
      } catch (e) { setError(e instanceof Error ? e.message : "Could not save schedule"); }
    }}>
      <Field label="Action"><NativeSelect value={kind} onChange={(e) => { setKind(e.target.value as typeof kind); setReplaces(""); }}>
        {catalog ? <option value="default">Schedule business default</option> : null}
        {commercial ? <><option value="assigned">Assign this book to customer</option><option value="override">Override one customer model</option></> : null}
      </NativeSelect></Field>
      {kind !== "default" ? <CustomerPicker value={customer} onChange={(id) => { setCustomer(id); setReplaces(""); }} /> : null}
      {kind === "override" ? <SaleFields value={override} onChange={setOverride} /> : <Field label="Published version"><NativeSelect value={version} onChange={(e) => setVersion(e.target.value)} required><option value="">Select version</option>{history.versions.map((v) => <option key={v.version} value={v.version}>v{v.version} · from draft {v.draftRevision}</option>)}</NativeSelect></Field>}
      <div className="grid gap-4 sm:grid-cols-2"><Field label="Start (UTC)" hint="Blank starts when the server commits; future dates end in Z."><TextInput value={from} onChange={(e) => setFrom(e.target.value)} placeholder="2026-12-01T00:00:00Z" /></Field><Field label="End (UTC)" hint="Blank stays effective until replaced or ended."><TextInput value={until} onChange={(e) => setUntil(e.target.value)} placeholder="2027-01-01T00:00:00Z" /></Field></div>
      <Field label="Replace an existing schedule" hint="Replacement ends the selected window atomically at the new start. Choose none only when there is no overlap."><NativeSelect value={replaces} disabled={unavailable} onChange={(e) => setReplaces(e.target.value)}><option value="">No replacement</option>{existing.filter((s) => ["Active", "Scheduled"].includes(scheduleState(s))).map((s) => <option key={s.id} value={s.id}>{s.bookId ? directory.data?.find((b) => b.id === s.bookId)?.name ?? s.bookId : s.model} · {s.effectiveFrom} · {s.id}</option>)}</NativeSelect></Field>
      {unavailable ? <Banner tone="warn">Load the relevant customer and schedule history before saving.</Banner> : null}
      <Field label="Reason"><TextInput value={reason} maxLength={500} required onChange={(e) => setReason(e.target.value)} /></Field>
      {error ? <Banner tone="error">{error} Your draft is retained.</Banner> : null}{saved ? <Banner tone="success">{saved}</Banner> : null}
      <Button type="submit" isLoading={mutation.isPending} disabled={unavailable || !reason.trim()}>Save activation or customer terms</Button>
    </form> : null}
    <h3 className="font-semibold">Business default history</h3>
    {directory.isLoading || histories.some((q) => q.isLoading) ? <p>Loading default history…</p> : directory.isError || histories.some((q) => q.isError) ? <Banner tone="error">Default history could not be loaded.</Banner> : <ScheduleRows rows={defaults} />}
    {customer ? <><h3 className="font-semibold">Selected customer history</h3>{customerTerms.isLoading ? <p>Loading customer history…</p> : customerTerms.isError ? <Banner tone="error">Customer history could not be loaded.</Banner> : <ScheduleRows rows={customerTerms.data ?? []} />}</> : null}
  </div></Card>;
}
