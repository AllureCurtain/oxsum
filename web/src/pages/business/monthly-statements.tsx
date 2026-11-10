import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ApiError } from "../../api/client";
import { idem, useBusiness, useCapability, useCustomerMonthlyStatement, useCustomerMonthlyStatements, useMonthlyStatementAction, type MonthlyStatementAction, type BusinessMonthlyStatementDetail } from "../../api/hooks";
import { AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle, Banner, Button, Card, Cr, EmptyRows, Field, TableShell, Td, Textarea, TextInput } from "../../components/ui";
import { DetailGrid, Field2 } from "./biz";

function MonthlyDetail({ customer, id, canWrite }: { customer: string; id: string; canWrite: boolean }) {
  const { t } = useTranslation();
  const query = useCustomerMonthlyStatement(customer, id);
  const detail = query.data;
  return <Card title={t("monthlyStatements.detail")} actions={<Button tone="ghost" disabled={query.isFetching} onClick={() => void query.refetch()}>{t("monthlyStatements.refresh")}</Button>}>
    {query.isError ? <Banner tone="error">{t("biz.loadError")}</Banner> : query.isLoading ? <p>{t("common.loading")}</p> : detail ? <div className="flex flex-col gap-4">
      <DetailGrid>
        <Field2 label={t("monthlyStatements.id")}><span className="break-all font-mono text-xs">{detail.statement.id}</span></Field2>
        <Field2 label={t("monthlyStatements.period")}>{detail.statement.period}</Field2>
        <Field2 label={t("monthlyStatements.document")}>{t(`monthlyStatements.${detail.statement.status}`)}</Field2>
        <Field2 label={t("monthlyStatements.payment")}>{t(`monthlyStatements.${detail.statement.paymentStatus}`)}</Field2>
        <Field2 label={t("monthlyStatements.usage")}><Cr minor={detail.statement.totalMinor} /></Field2>
        <Field2 label={t("monthlyStatements.drawn")}><Cr minor={detail.statement.creditDrawnMinor} /></Field2>
        <Field2 label={t("monthlyStatements.paidAmount")}><Cr minor={detail.statement.paidMinor} /></Field2>
        <Field2 label={t("monthlyStatements.owed")}><Cr minor={detail.statement.outstandingMinor} /></Field2>
        <Field2 label={t("monthlyStatements.terms")}>{detail.statement.paymentTermsDays}</Field2>
        <Field2 label={t("monthlyStatements.due")}>{detail.statement.dueDate ?? "—"}</Field2>
        <Field2 label={t("monthlyStatements.issued")}>{detail.statement.finalizedAt ? <><span className="whitespace-nowrap">{detail.statement.finalizedAt.slice(0, 10)}</span>{" "}<span className="whitespace-nowrap">{detail.statement.finalizedAt.slice(11)}</span></> : "—"}</Field2>
      </DetailGrid>
      {canWrite ? <MonthlyAction customer={customer} detail={detail} onDone={() => void query.refetch()} /> : null}
      <TableShell className="[&_table]:min-w-[480px]" head={[t("monthlyStatements.channel"), t("common.model"), t("common.turns"), { label: t("common.amount"), align: "right" }]}>
        {detail.lines.map((line, index) => <tr key={index}><td>{line.channel}</td><td>{line.model}</td><Td mono>{line.turns}</Td><Td mono align="right" className="whitespace-nowrap"><Cr minor={line.amountMinor} /></Td></tr>)}
        {detail.lines.length === 0 ? <EmptyRows colSpan={4}>{t("monthlyStatements.noLines")}</EmptyRows> : null}
      </TableShell>
      <details className="text-sm">
        <summary className="cursor-pointer text-primary">{t("monthlyStatements.proof")}</summary>
        <p className="my-3 text-muted">{t("monthlyStatements.proofHint")}</p>
        <DetailGrid>
          <Field2 label={t("monthlyStatements.anchor")}><span className="break-all font-mono text-xs">{detail.statement.anchorEntryId ?? "—"}</span></Field2>
          <Field2 label={t("monthlyStatements.window")}>{detail.statement.logFromIndex ?? "—"} → {detail.statement.logToIndex ?? "—"}</Field2>
          <Field2 label={t("monthlyStatements.windowHash")}><span className="break-all font-mono text-xs">{detail.statement.windowHash ?? "—"}</span></Field2>
          <Field2 label={t("monthlyStatements.rollupHash")}><span className="break-all font-mono text-xs">{detail.statement.rollupHash ?? "—"}</span></Field2>
        </DetailGrid>
      </details>
    </div> : null}
  </Card>;
}

export function CustomerMonthlyStatements({ customer, active }: { customer: string; active: boolean }) {
  const { t } = useTranslation();
  const [after, setAfter] = useState<string>();
  const [selected, setSelected] = useState<string>();
  const query = useCustomerMonthlyStatements(customer, after);
  const canWrite = useCapability(useBusiness().data, "fundsWrite") && active;
  return <div className="flex flex-col gap-4">
    <Card title={t("monthlyStatements.title")}>
      <p className="mb-4 text-sm text-muted">{t("monthlyStatements.description")}</p>
      <p className="mb-4 text-sm text-muted">{t("monthlyStatements.recorded")}</p>
      {canWrite ? <MonthlyAction customer={customer} onDone={(id) => { setAfter(undefined); if (id) setSelected(id); }} /> : null}
      {query.isError ? <Banner tone="error">{t("biz.loadError")}</Banner> : <TableShell className="[&_table]:min-w-[640px]" head={[t("monthlyStatements.period"), t("monthlyStatements.document"), t("monthlyStatements.payment"), { label: t("monthlyStatements.owed"), align: "right" }, t("monthlyStatements.due"), ""]} loading={query.isLoading ? 3 : 0}>
        {query.data?.statements.map(statement => <tr key={statement.id}>
          <Td mono>{statement.period}</Td><td>{t(`monthlyStatements.${statement.status}`)}</td><td>{t(`monthlyStatements.${statement.paymentStatus}`)}</td><Td mono align="right" className="whitespace-nowrap"><Cr minor={statement.outstandingMinor} /></Td><Td mono>{statement.dueDate ?? "—"}</Td>
          <td><Button tone="ghost" aria-label={t("monthlyStatements.openPeriod", { period: statement.period })} onClick={() => setSelected(statement.id)}>{t("monthlyStatements.open")}</Button></td>
        </tr>)}
        {!query.isLoading && query.data?.statements.length === 0 ? <EmptyRows colSpan={6}>{t("monthlyStatements.empty")}</EmptyRows> : null}
      </TableShell>}
      <div className="mt-3 flex flex-wrap gap-2">
        <Button tone="ghost" onClick={() => void query.refetch()} disabled={query.isFetching}>{t("monthlyStatements.refresh")}</Button>
        {after ? <Button tone="ghost" onClick={() => { setAfter(undefined); setSelected(undefined); }}>{t("monthlyStatements.first")}</Button> : null}
        {query.data?.nextCursor ? <Button tone="ghost" onClick={() => { setAfter(query.data!.nextCursor!); setSelected(undefined); }}>{t("monthlyStatements.next")}</Button> : null}
      </div>
    </Card>
    {selected ? <MonthlyDetail key={selected} customer={customer} id={selected} canWrite={canWrite} /> : null}
  </div>;
}


function MonthlyAction({ customer, detail, onDone }: { customer: string; detail?: BusinessMonthlyStatementDetail; onDone: (id: string | null) => void }) {
  const { t } = useTranslation();
  const mutation = useMonthlyStatementAction(customer);
  const [period, setPeriod] = useState(() => { const date = new Date(); date.setUTCDate(1); date.setUTCMonth(date.getUTCMonth() - 1); return date.toISOString().slice(0, 7); });
  const [reason, setReason] = useState("");
  const [intent, setIntent] = useState<MonthlyStatementAction | null>(null);
  const [reviewed, setReviewed] = useState<BusinessMonthlyStatementDetail>();
  const [attempted, setAttempted] = useState(false);
  const [open, setOpen] = useState(false);
  const [saved, setSaved] = useState<string>();
  const [stale, setStale] = useState(false);
  const frozen = attempted && intent !== null;
  const finalizing = Boolean(detail);
  if (detail?.statement.status === "finalized" && !intent) return null;
  const submit = async () => {
    if (!intent) return;
    setAttempted(true);
    try {
      const result = await mutation.mutateAsync(intent);
      setIntent(null); setAttempted(false); setOpen(false); setReason("");
      setSaved(result.statementId ? t(finalizing ? "monthlyStatements.issuedSuccess" : "monthlyStatements.generatedSuccess") : t("monthlyStatements.emptyMonth"));
      onDone(result.statementId);
    } catch (error) {
      if (error instanceof ApiError && error.status === 400 && error.code === "VALIDATION_ERROR") {
        setAttempted(false);
      } else if (error instanceof ApiError && error.status === 409 && (error.message.startsWith("monthly statement changed since review") || error.message.startsWith("monthly statement is already finalized"))) {
        setIntent(null); setAttempted(false); setOpen(false); setStale(true);
      }
    }
  };
  return <div className="my-4 border-t border-line pt-4">
    {saved ? <Banner tone="success">{saved}</Banner> : null}
    {stale ? <Banner tone="warn">{t("monthlyStatements.stale")}</Banner> : null}
    <form className="flex flex-col gap-3" onSubmit={(event) => {
      event.preventDefault();
      if (!reason.trim()) return;
      if (!intent) {
        if (detail) {
          if (!detail.reviewDigest) return;
          setReviewed(detail);
          setIntent({ action: "finalize", statementId: detail.statement.id, reviewDigest: detail.reviewDigest, idempotencyKey: idem(), reason: reason.trim() });
        } else {
          setIntent({ action: "generate", period, idempotencyKey: idem(), reason: reason.trim() });
        }
      }
      setOpen(true); setSaved(undefined); setStale(false); mutation.reset();
    }}>
      {!finalizing ? <Field label={t("monthlyStatements.period")} hint={t("monthlyStatements.closedMonth")}><TextInput type="month" value={period} required disabled={frozen} onChange={(event) => { setPeriod(event.target.value); setIntent(null); }} /></Field> : null}
      <Field label={t("biz.reason")}><Textarea value={reason} maxLength={500} required disabled={frozen} onChange={(event) => { setReason(event.target.value); setIntent(null); }} /></Field>
      {frozen ? <Banner tone="warn">{t("monthlyStatements.retained")}</Banner> : null}
      <Button type="submit" disabled={!reason.trim() || (!finalizing && !/^\d{4}-\d{2}$/.test(period)) || mutation.isPending}>{t(frozen ? "monthlyStatements.retry" : finalizing ? "monthlyStatements.reviewIssue" : "monthlyStatements.generate")}</Button>
    </form>
    <AlertDialog open={open} onOpenChange={(next) => { if (!mutation.isPending) setOpen(next); }}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{t(finalizing ? "monthlyStatements.confirmIssue" : "monthlyStatements.confirmGenerate")}</AlertDialogTitle>
          <AlertDialogDescription>{t(finalizing ? "monthlyStatements.issueHint" : "monthlyStatements.generateHint")}</AlertDialogDescription>
        </AlertDialogHeader>
        <DetailGrid>
          <Field2 label={t("monthlyStatements.period")}>{intent?.action === "generate" ? intent.period : reviewed?.statement.period}</Field2>
          {intent?.action === "finalize" && reviewed ? <>
            <Field2 label={t("monthlyStatements.usage")}><Cr minor={reviewed.statement.totalMinor} /></Field2>
            <Field2 label={t("monthlyStatements.drawn")}><Cr minor={reviewed.statement.creditDrawnMinor} /></Field2>
            <Field2 label={t("monthlyStatements.terms")}>{reviewed.statement.paymentTermsDays}</Field2>
          </> : null}
        </DetailGrid>
        <p className="break-words whitespace-pre-wrap text-sm">{intent?.reason}</p>
        {mutation.error ? <Banner tone="error">{mutation.error instanceof ApiError ? mutation.error.message : t("monthlyStatements.uncertain")}</Banner> : null}
        <AlertDialogFooter>
          <Button tone="ghost" disabled={mutation.isPending} onClick={() => setOpen(false)}>{t("common.cancel")}</Button>
          <Button isLoading={mutation.isPending} onClick={() => void submit()}>{t(frozen ? "monthlyStatements.retry" : "common.confirm")}</Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </div>;
}
