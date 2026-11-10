import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useCustomerMonthlyStatement, useCustomerMonthlyStatements } from "../../api/hooks";
import { Banner, Button, Card, Cr, EmptyRows, TableShell, Td } from "../../components/ui";
import { DetailGrid, Field2 } from "./biz";

function MonthlyDetail({ customer, id }: { customer: string; id: string }) {
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

export function CustomerMonthlyStatements({ customer }: { customer: string }) {
  const { t } = useTranslation();
  const [after, setAfter] = useState<string>();
  const [selected, setSelected] = useState<string>();
  const query = useCustomerMonthlyStatements(customer, after);
  return <div className="flex flex-col gap-4">
    <Card title={t("monthlyStatements.title")}>
      <p className="mb-4 text-sm text-muted">{t("monthlyStatements.description")}</p>
      <p className="mb-4 text-sm text-muted">{t("monthlyStatements.recorded")}</p>
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
    {selected ? <MonthlyDetail customer={customer} id={selected} /> : null}
  </div>;
}
