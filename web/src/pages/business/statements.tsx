import { useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { FileText, Receipt } from "lucide-react";
import {
  useBusiness,
  useBusinessJournals,
  useBusinessJournal,
  useBusinessStatement,
  useBusinessStatements,
  useCapability,
  useDisplay,
  useReleaseStatement,
} from "../../api/hooks";
import {
  Banner,
  Button,
  Card,
  Cr,
  EmptyRows,
  Kpi,
  TableShell,
  Td,
} from "../../components/ui";
import { InvoiceCorrection } from "./invoice-correction";
import { AuditAction, DetailGrid, Field2, stateBadge } from "./biz";
import { JournalVoid } from "./journal-void";

// ---------------------------------------------------------------------------
// /app/business#business/statements + #business/receivables — the plan-fee
// receivables (T21) and invoice journals (T20). A statement detail resolves
// through `release-review` (extendGrace/void); a journal voids under a reason.
// Receivables buckets issued|past_due statements by days-past-due.
// ---------------------------------------------------------------------------

function StatementRows() {
  const { t } = useTranslation();
  const display = useDisplay();
  const statements = useBusinessStatements({});
  const release = useReleaseStatement();
  const canWrite = useCapability(useBusiness().data, "fundsWrite");
  const [expanded, setExpanded] = useState<string | null>(null);

  return (
    <Card title={t("biz.statements.listTitle")}>
      {statements.isError ? (
        <Banner tone="error">{t("biz.loadError")}</Banner>
      ) : (
        <TableShell
          head={[
            t("biz.statements.colId"),
            t("biz.statements.colOrg"),
            t("biz.statements.colPeriod"),
            t("common.status"),
            { label: t("common.total"), align: "right" },
            { label: t("biz.statements.colOwed"), align: "right" },
            t("biz.statements.colDue"),
            "",
          ]}
          loading={statements.isLoading ? 5 : 0}
        >
          {!statements.isLoading &&
            (statements.data?.statements ?? []).map((s) => (
              <>
                <tr key={s.statementId} className="cursor-pointer" onClick={() => setExpanded(expanded === s.statementId ? null : s.statementId)}>
                  <Td mono truncate>{s.statementId.slice(0, 8)}</Td>
                  <Td mono truncate>{s.organizationId.slice(0, 8)}</Td>
                  <Td mono>{s.periodIndex ?? "—"}</Td>
                  <td>{stateBadge(s.state)}</td>
                  <Td mono align="right"><Cr minor={s.totalMinor} currency={display} /></Td>
                  <Td mono align="right"><Cr minor={s.totalMinor - s.paidMinor} currency={display} /></Td>
                  <Td mono>{s.dueAt?.slice(0, 10) ?? "—"}</Td>
                  <td className="text-right">
                    {canWrite && s.state === "pastDue" ? (
                      <AuditAction
                        trigger={<Button tone="ghost">{t("biz.statements.extendGrace")}</Button>}
                        title={t("biz.statements.graceTitle")}
                        description={t("biz.statements.graceExplain")}
                        confirmLabel={t("biz.statements.extendGrace")}
                        reasonLabel={t("biz.reason")}
                        isPending={release.isPending}
                        error={release.error}
                        onConfirm={(reason, key) =>
                          release.mutate({ statementId: s.statementId, idempotencyKey: key, action: "extendGrace", reason })
                        }
                      />
                    ) : null}
                    {canWrite && (s.state === "issued" || s.state === "pastDue") ? (
                      <AuditAction
                        trigger={<Button tone="danger">{t("biz.statements.void")}</Button>}
                        title={t("biz.statements.voidTitle")}
                        description={t("biz.statements.voidExplain")}
                        confirmLabel={t("biz.statements.void")}
                        tone="danger"
                        reasonLabel={t("biz.reason")}
                        isPending={release.isPending}
                        error={release.error}
                        onConfirm={(reason, key) =>
                          release.mutate({ statementId: s.statementId, idempotencyKey: key, action: "void", reason })
                        }
                      />
                    ) : null}
                  </td>
                </tr>
                {expanded === s.statementId ? (
                  <tr key={`${s.statementId}-d`}>
                    <td colSpan={8} className="bg-canvas-inset">
                      <DetailGrid>
                        <Field2 label={t("biz.statements.issued")}>{s.issuedAt?.slice(0, 10) ?? "—"}</Field2>
                        <Field2 label={t("biz.statements.paidAt")}>{s.paidAt?.slice(0, 10) ?? "—"}</Field2>
                        <Field2 label={t("biz.statements.pastDueAt")}>{s.pastDueAt?.slice(0, 10) ?? "—"}</Field2>
                        <Field2 label={t("biz.statements.journal")}>
                          <span className="mono text-xs">{s.journalId?.slice(0, 8) ?? "—"}</span>
                        </Field2>
                        {s.voidReason ? <Field2 label={t("biz.statements.voidReason")}>{s.voidReason}</Field2> : null}
                      </DetailGrid>
                    </td>
                  </tr>
                ) : null}
              </>
            ))}
          {!statements.isLoading && (statements.data?.statements.length ?? 0) === 0 ? (
            <EmptyRows colSpan={8} icon={<FileText />}>
              {t("biz.statements.empty")}
            </EmptyRows>
          ) : null}
        </TableShell>
      )}
    </Card>
  );
}

function JournalRows() {
  const { t } = useTranslation();
  const display = useDisplay();
  const journals = useBusinessJournals({});
  const canWrite = useCapability(useBusiness().data, "fundsWrite");

  return (
    <Card title={t("biz.journals.listTitle")}>
      {journals.isError ? (
        <Banner tone="error">{t("biz.loadError")}</Banner>
      ) : (
        <TableShell
          density="compact"
          head={[
            t("biz.journals.colId"),
            t("biz.journals.colOrg"),
            t("common.status"),
            { label: t("common.total"), align: "right" },
            { label: t("biz.journals.colSettled"), align: "right" },
            { label: t("biz.journals.colDisputed"), align: "right" },
            "",
          ]}
          loading={journals.isLoading ? 4 : 0}
        >
          {!journals.isLoading &&
            (journals.data?.journals ?? []).map((j) => (
              <tr key={j.journalId}>
                <Td mono truncate><Link className="text-primary" to={`/business?journal=${j.journalId}#business/statements`}>{j.journalId.slice(0, 8)}</Link></Td>
                <Td mono truncate>{j.organizationId.slice(0, 8)}</Td>
                <td>{stateBadge(j.state)}</td>
                <Td mono align="right"><Cr minor={j.totalMinor} currency={display} /></Td>
                <Td mono align="right"><Cr minor={j.settledMinor} currency={display} /></Td>
                <Td mono align="right"><Cr minor={j.disputedMinor} currency={display} /></Td>
                <td className="text-right">
                  {canWrite ? <InvoiceCorrection journalId={j.journalId} /> : null}
                  {canWrite ? <JournalVoid journal={j} /> : null}
                </td>
              </tr>
            ))}
          {!journals.isLoading && (journals.data?.journals.length ?? 0) === 0 ? (
            <EmptyRows colSpan={7}>{t("biz.journals.empty")}</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </Card>
  );
}

export function BusinessStatements() {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-6">
      <h2 className="text-xl font-bold">{t("biz.statements.title")}</h2>
      <StatementPaymentSources />
      <StatementRows />
      <JournalRows />
    </div>
  );
}

// ---------------------------------------------------------------------------
// receivables — aging of the open receivable: `issued`/`pastDue` statements
// bucketed by days past `dueAt`, plus the outstanding total. Derived from the
// same business statements read; no separate endpoint needed.
// ---------------------------------------------------------------------------

export function BusinessReceivables() {
  const { t } = useTranslation();
  const display = useDisplay();
  const statements = useBusinessStatements({});
  const open = (statements.data?.statements ?? []).filter(
    (s) => s.state === "issued" || s.state === "pastDue",
  );
  const now = Date.now();
  const bucket = (s: { dueAt: string | null; totalMinor: number; paidMinor: number }) => {
    if (!s.dueAt) return 0;
    return Math.floor((now - Date.parse(s.dueAt)) / 86_400_000);
  };
  const buckets = [
    { label: t("biz.receivables.current"), rows: open.filter((s) => bucket(s) <= 0) },
    { label: t("biz.receivables.d30"), rows: open.filter((s) => bucket(s) > 0 && bucket(s) <= 30) },
    { label: t("biz.receivables.d60"), rows: open.filter((s) => bucket(s) > 30 && bucket(s) <= 60) },
    { label: t("biz.receivables.d90"), rows: open.filter((s) => bucket(s) > 60) },
  ];
  const total = open.reduce((acc, s) => acc + (s.totalMinor - s.paidMinor), 0);

  return (
    <div className="flex flex-col gap-6">
      <h2 className="text-xl font-bold">{t("biz.receivables.title")}</h2>
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        {buckets.map((b) => (
          <Kpi
            key={b.label}
            label={b.label}
            icon={<Receipt />}
            value={<Cr minor={b.rows.reduce((a, s) => a + (s.totalMinor - s.paidMinor), 0)} currency={display} />}
            hint={t("biz.receivables.count", { count: b.rows.length })}
          />
        ))}
      </div>
      <Card title={t("biz.receivables.openTitle")} actions={<span className="text-sm font-semibold tabular-nums"><Cr minor={total} currency={display} /></span>}>
        {statements.isError ? (
          <Banner tone="error">{t("biz.loadError")}</Banner>
        ) : (
          <TableShell
            head={[
              t("biz.statements.colId"),
              t("biz.statements.colOrg"),
              t("common.status"),
              { label: t("biz.receivables.colOwed"), align: "right" },
              { label: t("biz.receivables.colDaysPast"), align: "right" },
              t("biz.statements.colDue"),
            ]}
            loading={statements.isLoading ? 5 : 0}
          >
            {!statements.isLoading &&
              open.map((s) => (
                <tr key={s.statementId}>
                  <Td mono truncate>{s.statementId.slice(0, 8)}</Td>
                  <Td mono truncate>{s.organizationId.slice(0, 8)}</Td>
                  <td>{stateBadge(s.state)}</td>
                  <Td mono align="right"><Cr minor={s.totalMinor - s.paidMinor} currency={display} /></Td>
                  <Td mono align="right">{Math.max(0, bucket(s))}</Td>
                  <Td mono>{s.dueAt?.slice(0, 10) ?? "—"}</Td>
                </tr>
              ))}
            {!statements.isLoading && open.length === 0 ? (
              <EmptyRows colSpan={6} icon={<Receipt />}>
                {t("biz.receivables.empty")}
              </EmptyRows>
            ) : null}
          </TableShell>
        )}
      </Card>
    </div>
  );
}

/** Source links from finance payments open the exact document, independent of pagination. */
export function StatementPaymentSources() {
  const { t } = useTranslation();
  const [params] = useSearchParams();
  const statement = useBusinessStatement(params.get("statement") ?? undefined);
  const journal = useBusinessJournal(params.get("journal") ?? undefined);
  if (!params.has("statement") && !params.has("journal")) return null;
  const query = params.has("journal") ? journal : statement;
  const document = query.data;
  return <Card title={t("statementPayment.allocations")}>
    {query.isLoading ? <p>{t("common.loading")}</p> : query.error ? <Banner tone="error">{query.error.message}</Banner>
      : document ? <div className="flex flex-col gap-3">
        <DetailGrid>
          <Field2 label={t("common.status")}>{stateBadge(document.state)}</Field2>
          <Field2 label={t("common.total")}><Cr minor={document.totalMinor} /></Field2>
          <Field2 label={t("biz.statements.colOrg")}><span className="font-mono text-xs">{document.organizationId}</span></Field2>
        </DetailGrid>
        {params.has("journal") && journal.data ? <div className="flex flex-wrap gap-3">
          {journal.data.replacesJournalId ? <Link className="text-primary" to={`/business?journal=${journal.data.replacesJournalId}#business/statements`}>{t("invoiceCorrection.predecessor")}</Link> : null}
          {journal.data.successorJournalId ? <Link className="text-primary" to={`/business?journal=${journal.data.successorJournalId}#business/statements`}>{t("invoiceCorrection.successor")}</Link> : null}
          <InvoiceCorrection key={journal.data.journalId} journalId={journal.data.journalId} />
        </div> : statement.data ? <div className="flex flex-wrap gap-3">
          {statement.data.replacesStatementId ? <Link className="text-primary" to={`/business?statement=${statement.data.replacesStatementId}#business/statements`}>{t("invoiceCorrection.predecessor")}</Link> : null}
          {statement.data.successorStatementId ? <Link className="text-primary" to={`/business?statement=${statement.data.successorStatementId}#business/statements`}>{t("invoiceCorrection.successor")}</Link> : null}
        </div> : null}
        <TableShell head={[t("billing.colKind"), t("billing.colDescription"), { label: t("common.amount"), align: "right" }, t("biz.payments.reference")]}>
          {document.lines.map((line) => <tr key={line.lineId}>
            <td>{line.kind}</td><td>{line.description}</td><Td mono align="right"><Cr minor={line.amountMinor} /></Td><Td mono>{line.sourceRef ?? "—"}</Td>
          </tr>)}
        </TableShell>
      </div> : null}
  </Card>;
}
