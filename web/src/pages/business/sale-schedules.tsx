import { useTranslation } from "react-i18next";
import { saleError, saleLabel } from "../../lib/sale-text";
import { useEffect, useState } from "react";
import { useQueries } from "@tanstack/react-query";
import { api } from "../../api/client";
import { useBusiness, useBusinessCustomers } from "../../api/hooks";
import { useCustomerSaleTerms, useSaleChange, useSaleDirectory, type SaleChange, type SaleHistory, type SaleSchedule } from "../../api/hooks/sales";
import { Banner, Button, Card, Field, NativeSelect, TableShell, Td, TextInput, EmptyRows, AlertDialog, AlertDialogContent, AlertDialogTitle, AlertDialogDescription } from "../../components/ui";
import { modelDraft, scheduleState, termsValue, utc, utcMicros } from "../../lib/sale-draft";
import { SaleFields } from "./sale-fields";

export function CustomerPicker({ value, onChange }: { value: string; onChange: (id: string) => void }) {
  const { t } = useTranslation();
  const [cursor, setCursor] = useState<string>();
  const [label, setLabel] = useState("");
  const customers = useBusinessCustomers(cursor);
  return <div className="flex flex-col gap-3">
    <Field label={t("sale.customer")}>
      <NativeSelect value={value} disabled={customers.isLoading} onChange={(e) => {
        onChange(e.target.value); setLabel(customers.data?.customers.find((c) => c.organizationId === e.target.value)?.name ?? "");
      }}>
        <option value="">{t("sale.selectCustomer")}</option>
        {value && !customers.data?.customers.some((c) => c.organizationId === value) ? <option value={value}>{label || value}</option> : null}
        {customers.data?.customers.map((c) => <option key={c.organizationId} value={c.organizationId}>{c.name} · {c.externalId} · {saleLabel(c.state, t)}</option>)}
      </NativeSelect>
    </Field>
    {customers.isError ? <Banner tone="error">{t("sale.loadCustomersError")}</Banner> : null}
    <div className="flex gap-3"><Button type="button" tone="ghost" disabled={!cursor || customers.isFetching} onClick={() => setCursor(undefined)}>{t("sale.firstCustomers")}</Button>
      <Button type="button" tone="ghost" disabled={!customers.data?.nextCursor || customers.isFetching} onClick={() => setCursor(customers.data?.nextCursor ?? undefined)}>{t("sale.moreCustomers")}</Button></div>
  </div>;
}

export function ScheduleRows({ rows }: { rows: SaleSchedule[] }) {
  const { t } = useTranslation();
  const [now, setNow] = useState(Date.now());
  useEffect(() => { const timer = setInterval(() => setNow(Date.now()), 1000); return () => clearInterval(timer); }, []);
  const principal = useBusiness();
  const change = useSaleChange();
  const [ending, setEnding] = useState<SaleSchedule | null>(null);
  const [until, setUntil] = useState("");
  const [reason, setReason] = useState("");
  const [error, setError] = useState("");
  return <>
    <TableShell head={[t("sale.state"), t("sale.scope"), t("sale.versionModel"), t("sale.from"), t("sale.until"), t("sale.action")]}>
      {rows.map((row) => <tr key={row.id}>
        <Td>{saleLabel(scheduleState(row, now), t)}</Td><Td>{saleLabel(row.kind, t)}{row.organizationId ? ` · ${row.organizationId}` : ""}</Td>
        <Td mono>{row.model ?? `v${row.version}`}<span className="block text-xs">{row.bookId}</span></Td>
        <Td mono>{row.effectiveFrom}</Td><Td mono>{row.effectiveUntil ?? t("sale.openEnded")}</Td>
        <Td>{principal.data?.capabilities.includes(row.kind === "default" ? "catalogWrite" : "commercialWrite") && ["Active", "Scheduled"].includes(scheduleState(row, now)) ? <Button type="button" tone="ghost" onClick={() => {
          setEnding(row); setUntil(scheduleState(row) === "Scheduled" ? row.effectiveFrom : new Date(Date.now() + 60_000).toISOString()); setReason(""); setError(""); change.reset();
        }}>{t("sale.endCancel")}</Button> : "—"}</Td>
      </tr>)}
      {!rows.length ? <EmptyRows colSpan={6}>{t("sale.noSchedules")}</EmptyRows> : null}
    </TableShell>
    <AlertDialog open={ending != null} onOpenChange={(open) => { if (!open && !change.isPending) setEnding(null); }}>
      <AlertDialogContent><AlertDialogTitle>{t("sale.endTitle")}</AlertDialogTitle>
        <AlertDialogDescription>{t("sale.endDescription", { kind: saleLabel(ending?.kind ?? "", t), id: ending?.id })}</AlertDialogDescription>
        <Field label={t("sale.endUtc")}><TextInput value={until} onChange={(e) => setUntil(e.target.value)} /></Field>
        <Field label={t("sale.reason")}><TextInput value={reason} maxLength={500} onChange={(e) => setReason(e.target.value)} /></Field>
        {error ? <Banner tone="error">{error}</Banner> : null}
        <div className="flex gap-3"><Button type="button" tone="ghost" disabled={change.isPending} onClick={() => setEnding(null)}>{t("sale.keepSchedule")}</Button>
          <Button type="button" tone="danger" isLoading={change.isPending} disabled={!reason.trim()} onClick={async () => {
            try { const end = utc(until); if (!end || !ending) throw new Error(t("sale.exactEndRequired"));
              await change.submit({ operation: "endSchedule", scheduleId: ending.id, effectiveUntil: end }, reason); setEnding(null);
            } catch (e) { setError(saleError(e, t, t("sale.endError"))); }
          }}>{t("sale.confirmEnd")}</Button></div>
      </AlertDialogContent>
    </AlertDialog>
  </>;
}

export function SaleSchedules({ history }: { history: SaleHistory }) {
  const { t } = useTranslation();
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
  return <Card title={t("sale.activationTitle")}><div className="flex flex-col gap-4">
    <p>{t("sale.activationHint")}</p>
    {(catalog || commercial) ? <form className="flex flex-col gap-4" onSubmit={async (event) => {
      event.preventDefault(); setError(""); setSaved("");
      try {
        const effectiveFrom = utc(from), effectiveUntil = utc(until);
        if (effectiveFrom && effectiveUntil && utcMicros(effectiveFrom) >= utcMicros(effectiveUntil)) throw new Error(t("sale.endAfterStart"));
        const window = { effectiveFrom, effectiveUntil, replaces: replaces || null };
        let change: SaleChange;
        if (kind === "override") change = { operation: "overrideModel", organizationId: customer, model: override.model.trim(), terms: termsValue(override), ...window };
        else { const selected = history.versions.find((v) => String(v.version) === version); if (!selected) throw new Error(t("sale.selectPublished"));
          change = kind === "default" ? { operation: "scheduleDefault", bookId: history.book.id, version: selected.version, ...window } : { operation: "assignBook", organizationId: customer, bookId: history.book.id, version: selected.version, ...window }; }
        const result = await mutation.submit(change, reason); setSaved(t("sale.savedSchedule", { id: result.resourceId }));
      } catch (e) { setError(saleError(e, t, t("sale.saveScheduleError"))); }
    }}>
      <Field label={t("sale.action")}><NativeSelect value={kind} onChange={(e) => { setKind(e.target.value as typeof kind); setReplaces(""); }}>
        {catalog ? <option value="default">{t("sale.scheduleDefault")}</option> : null}
        {commercial ? <><option value="assigned">{t("sale.assignBook")}</option><option value="override">{t("sale.overrideModel")}</option></> : null}
      </NativeSelect></Field>
      {kind !== "default" ? <CustomerPicker value={customer} onChange={(id) => { setCustomer(id); setReplaces(""); }} /> : null}
      {kind === "override" ? <SaleFields value={override} onChange={setOverride} /> : <Field label={t("sale.publishedVersion")}><NativeSelect value={version} onChange={(e) => setVersion(e.target.value)} required><option value="">{t("sale.selectVersion")}</option>{history.versions.map((v) => <option key={v.version} value={v.version}>{t("sale.scheduleVersion", { version: v.version, revision: v.draftRevision })}</option>)}</NativeSelect></Field>}
      <div className="grid gap-4 sm:grid-cols-2"><Field label={t("sale.startUtc")} hint={t("sale.startHint")}><TextInput value={from} onChange={(e) => setFrom(e.target.value)} placeholder="2026-12-01T00:00:00Z" /></Field><Field label={t("sale.endUtc")} hint={t("sale.endHint")}><TextInput value={until} onChange={(e) => setUntil(e.target.value)} placeholder="2027-01-01T00:00:00Z" /></Field></div>
      <Field label={t("sale.replaceSchedule")} hint={t("sale.replaceHint")}><NativeSelect value={replaces} disabled={unavailable} onChange={(e) => setReplaces(e.target.value)}><option value="">{t("sale.noReplacement")}</option>{existing.filter((s) => ["Active", "Scheduled"].includes(scheduleState(s))).map((s) => <option key={s.id} value={s.id}>{s.bookId ? directory.data?.find((b) => b.id === s.bookId)?.name ?? s.bookId : s.model} · {s.effectiveFrom} · {s.id}</option>)}</NativeSelect></Field>
      {unavailable ? <Banner tone="warn">{t("sale.loadBeforeSave")}</Banner> : null}
      <Field label={t("sale.reason")}><TextInput value={reason} maxLength={500} required onChange={(e) => setReason(e.target.value)} /></Field>
      {error ? <Banner tone="error">{error} {t("sale.draftRetained")}</Banner> : null}{saved ? <Banner tone="success">{saved}</Banner> : null}
      <Button type="submit" isLoading={mutation.isPending} disabled={unavailable || !reason.trim()}>{t("sale.saveActivation")}</Button>
    </form> : null}
    <h3 className="font-semibold">{t("sale.defaultHistory")}</h3>
    {directory.isLoading || histories.some((q) => q.isLoading) ? <p>{t("sale.loadingDefaults")}</p> : directory.isError || histories.some((q) => q.isError) ? <Banner tone="error">{t("sale.loadDefaultsError")}</Banner> : <ScheduleRows rows={defaults} />}
    {customer ? <><h3 className="font-semibold">{t("sale.customerHistory")}</h3>{customerTerms.isLoading ? <p>{t("sale.loadingCustomerHistory")}</p> : customerTerms.isError ? <Banner tone="error">{t("sale.loadCustomerHistoryError")}</Banner> : <ScheduleRows rows={customerTerms.data ?? []} />}</> : null}
  </div></Card>;
}
