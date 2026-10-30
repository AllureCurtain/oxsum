import { Link, useParams } from "react-router-dom";
import { useDisplay, useStatement, useStatements } from "../api/hooks";
import { Card, Cr, EmptyRows, PageState, StatusBadge, TableShell, Td } from "../components/ui";

export function Statements() {
  const statements = useStatements();
  const display = useDisplay();
  return (
    <div className="flex flex-col gap-4">
      <h1 className="text-2xl font-bold">Statements</h1>
      {statements.isError ? (
        <PageState>The statements could not be read.</PageState>
      ) : (
        <TableShell
          head={[
            "Period",
            "Status",
            "Payment",
            { label: "Total", align: "right" },
            { label: "Drawn", align: "right" },
            { label: "Outstanding", align: "right" },
            "Due",
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
                    detail
                  </Link>
                </td>
              </tr>
            ))}
          {!statements.isLoading && (statements.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={8}>No statements yet — statements appear once a period finalizes.</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </div>
  );
}

export function StatementDetail() {
  const { id } = useParams<{ id: string }>();
  const detail = useStatement(id ?? "");
  const display = useDisplay();
  const s = detail.data?.statement;

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Statement {s?.period}</h1>
        {s?.status === "finalized" ? (
          <a
            href={`/api/v1/statements/${s.id}/export.pdf`}
            className="text-primary"
            download
          >
            Export PDF
          </a>
        ) : null}
      </div>

      {detail.isError ? (
        <PageState>The statement could not be read.</PageState>
      ) : detail.isLoading ? (
        <PageState>Loading…</PageState>
      ) : s ? (
        <>
          <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
            <div className="rounded-md border border-line bg-surface p-4">
              <p className="text-xs text-muted">Total</p>
              <p className="mt-1 text-xl font-semibold mono"><Cr minor={s.totalMinor} currency={display} /></p>
            </div>
            <div className="rounded-md border border-line bg-surface p-4">
              <p className="text-xs text-muted">Credit drawn</p>
              <p className="mt-1 text-xl font-semibold mono"><Cr minor={s.creditDrawnMinor} currency={display} /></p>
            </div>
            <div className="rounded-md border border-line bg-surface p-4">
              <p className="text-xs text-muted">Paid</p>
              <p className="mt-1 text-xl font-semibold mono"><Cr minor={s.paidMinor} currency={display} /></p>
            </div>
            <div className="rounded-md border border-line bg-surface p-4">
              <p className="text-xs text-muted">Outstanding</p>
              <p className="mt-1 text-xl font-semibold mono"><Cr minor={s.outstandingMinor} currency={display} /></p>
            </div>
          </div>

          <dl className="grid grid-cols-2 gap-2 text-sm sm:grid-cols-4">
            <dt className="text-muted">Status</dt>
            <dd>
              <StatusBadge status={s.status} /> / <StatusBadge status={s.paymentStatus} />
            </dd>
            <dt className="text-muted">Due</dt>
            <dd>{s.dueDate ?? "—"}</dd>
            <dt className="text-muted">Finalized</dt>
            <dd>{s.finalizedAt ? String(s.finalizedAt).slice(0, 10) : "—"}</dd>
            <dt className="text-muted">Terms</dt>
            <dd>{s.paymentTermsDays} days</dd>
          </dl>

          <Card title="Lines">
            <TableShell
              head={[
                "Channel",
                "Model",
                { label: "Turns", align: "right" },
                { label: "In", align: "right" },
                { label: "Out", align: "right" },
                { label: "Cached", align: "right" },
                { label: "Reasoning", align: "right" },
                { label: "Amount", align: "right" },
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
                <EmptyRows colSpan={8}>No lines.</EmptyRows>
              ) : null}
            </TableShell>
          </Card>

          {detail.data?.anchor ? (
            <Card title="Log anchor">
              <AnchorView anchor={detail.data.anchor as Record<string, unknown>} />
            </Card>
          ) : null}
        </>
      ) : null}
      <p>
        <Link to="/statements" className="text-primary text-sm">
          ← Back to statements
        </Link>
      </p>
    </div>
  );
}

function AnchorView({ anchor }: { anchor: Record<string, unknown> }) {
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
          <dt className="text-muted">signed note</dt>
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
