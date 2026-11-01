import { Link, useParams } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { useDisplay, useStatement, useStatements } from "../api/hooks";
import { AlertCircle, CheckCircle2, CreditCard, FileText } from "lucide-react";
import { Card, Cr, EmptyRows, Kpi, PageState, StatusBadge, TableShell, Td } from "../components/ui";

export function Statements() {
  const { t } = useTranslation();
  const statements = useStatements();
  const display = useDisplay();
  return (
    <div className="flex flex-col gap-4">
      <h1 className="text-2xl font-bold">{t("statements.title")}</h1>
      {statements.isError ? (
        <PageState>{t("statements.listError")}</PageState>
      ) : (
        <TableShell
          head={[
            t("statements.colPeriod"),
            t("common.status"),
            t("statements.colPayment"),
            { label: t("common.total"), align: "right" },
            { label: t("statements.colDrawn"), align: "right" },
            { label: t("statements.outstanding"), align: "right" },
            t("statements.due"),
            "",
          ]}
          loading={statements.isLoading ? 4 : 0}
        >
          {!statements.isLoading &&
            (statements.data ?? []).map((s) => (
              <tr key={s.id}>
                <td>{s.period}</td>
                <td>
                  <StatusBadge status={s.status} />
                </td>
                <td>
                  <StatusBadge status={s.paymentStatus} />
                </td>
                <Td mono align="right"><Cr minor={s.totalMinor} currency={display} /></Td>
                <Td mono align="right"><Cr minor={s.creditDrawnMinor} currency={display} /></Td>
                <Td mono align="right"><Cr minor={s.outstandingMinor} currency={display} /></Td>
                <td>{s.dueDate ?? "—"}</td>
                <td>
                  <Link to={`/statements/${s.id}`} className="text-primary">
                    {t("statements.detailLink")}
                  </Link>
                </td>
              </tr>
            ))}
          {!statements.isLoading && (statements.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={8} icon={<FileText />}>
              {t("statements.empty")}
            </EmptyRows>
          ) : null}
        </TableShell>
      )}
    </div>
  );
}

export function StatementDetail() {
  const { t } = useTranslation();
  const { id } = useParams<{ id: string }>();
  const detail = useStatement(id ?? "");
  const display = useDisplay();
  const s = detail.data?.statement;

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">
          {t("statements.detailTitle", { period: s?.period })}
        </h1>
        {s?.status === "finalized" ? (
          <a
            href={`/api/v1/statements/${s.id}/export.pdf`}
            className="text-primary"
            download
          >
            {t("statements.exportPdf")}
          </a>
        ) : null}
      </div>

      {detail.isError ? (
        <PageState>{t("statements.detailError")}</PageState>
      ) : detail.isLoading ? (
        <PageState>{t("common.loading")}</PageState>
      ) : s ? (
        <>
          <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
            <Kpi label={t("statements.total")} icon={<FileText />} value={<Cr minor={s.totalMinor} currency={display} />} />
            <Kpi
              label={t("statements.creditDrawn")}
              icon={<CreditCard />}
              value={<Cr minor={s.creditDrawnMinor} currency={display} />}
            />
            <Kpi
              label={t("statements.paid")}
              icon={<CheckCircle2 />}
              value={<Cr minor={s.paidMinor} currency={display} />}
            />
            <Kpi
              label={t("statements.outstanding")}
              icon={<AlertCircle />}
              value={<Cr minor={s.outstandingMinor} currency={display} />}
            />
          </div>

          <dl className="grid grid-cols-2 gap-2 text-sm sm:grid-cols-4">
            <dt className="text-muted">{t("common.status")}</dt>
            <dd>
              <StatusBadge status={s.status} /> / <StatusBadge status={s.paymentStatus} />
            </dd>
            <dt className="text-muted">{t("statements.due")}</dt>
            <dd>{s.dueDate ?? "—"}</dd>
            <dt className="text-muted">{t("statements.finalized")}</dt>
            <dd>{s.finalizedAt ? String(s.finalizedAt).slice(0, 10) : "—"}</dd>
            <dt className="text-muted">{t("statements.terms")}</dt>
            <dd>{t("statements.termsDays", { days: s.paymentTermsDays })}</dd>
          </dl>

          <Card title={t("statements.lines")}>
            <TableShell
              head={[
                t("common.channel"),
                t("common.model"),
                { label: t("common.turns"), align: "right" },
                { label: t("statements.colIn"), align: "right" },
                { label: t("statements.colOut"), align: "right" },
                { label: t("statements.colCached"), align: "right" },
                { label: t("statements.colReasoning"), align: "right" },
                { label: t("statements.colAmount"), align: "right" },
              ]}
            >
              {(detail.data?.lines ?? []).map((line, i) => (
                <tr key={`${line.channel}-${line.model}-${i}`}>
                  <td>{line.channel}</td>
                  <td>{line.model}</td>
                  <Td mono align="right">{line.turns}</Td>
                  <Td mono align="right">{line.inputTokens}</Td>
                  <Td mono align="right">{line.outputTokens}</Td>
                  <Td mono align="right">{line.cachedTokens}</Td>
                  <Td mono align="right">{line.reasoningTokens}</Td>
                  <Td mono align="right"><Cr minor={line.amountMinor} currency={display} /></Td>
                </tr>
              ))}
              {(detail.data?.lines.length ?? 0) === 0 ? (
                <EmptyRows colSpan={8} icon={<FileText />}>{t("statements.noLines")}</EmptyRows>
              ) : null}
            </TableShell>
          </Card>

          {detail.data?.anchor ? (
            <Card title={t("statements.logAnchor")}>
              <AnchorView anchor={detail.data.anchor as Record<string, unknown>} />
            </Card>
          ) : null}
        </>
      ) : null}
      <p>
        <Link to="/statements" className="text-primary text-sm">
          {t("statements.back")}
        </Link>
      </p>
    </div>
  );
}

function AnchorView({ anchor }: { anchor: Record<string, unknown> }) {
  const { t } = useTranslation();
  return (
    <dl className="grid grid-cols-1 gap-1 text-sm sm:grid-cols-2">
      {(["entryId", "logIndex", "windowHash", "rollupHash", "origin"] as const).map((field) => (
        <div key={field} className="flex gap-2">
          <dt className="w-28 shrink-0 text-muted">{field}</dt>
          <dd className="mono">{String(anchor[field] ?? "—")}</dd>
        </div>
      ))}
      {anchor.note ? (
        <div className="col-span-2">
          <dt className="text-muted">{t("statements.signedNote")}</dt>
          <dd>
            <code className="mono block whitespace-pre-wrap break-all rounded bg-canvas p-2 text-xs">
              {String(anchor.note)}
            </code>
          </dd>
        </div>
      ) : null}
    </dl>
  );
}
