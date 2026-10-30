import { useState } from "react";
import {
  Banner,
  Button,
  EmptyRows,
  Field,
  NumberInput,
  TableShell,
  credits,
} from "../../components/ui";
import { ApiError } from "../../api/client";
import { adminApi, useAdmin } from "./api";

type AdminStatement = {
  id: string;
  organizationId: string;
  period: string;
  status: string;
  paymentStatus: string;
  totalMinor: number;
  creditDrawnMinor: number;
  outstandingMinor: number;
};

export function AdminStatements({ token }: { token: string }) {
  const statements = useAdmin<AdminStatement[]>(token, "/statements");
  return (
    <div>
      {statements.error ? (
        <Banner tone="error">{statements.error}</Banner>
      ) : (
        <TableShell
          head={[
            "Period",
            "Organization",
            "Status",
            "Payment",
            { label: "Total", align: "right" },
            { label: "Outstanding", align: "right" },
            "",
          ]}
          density="compact"
          loading={statements.loading ? 5 : 0}
        >
          {!statements.loading &&
            (statements.data ?? []).map((s) => (
            <StatementRow key={s.id} token={token} s={s} onDone={statements.refresh} />
          ))}
          {!statements.loading && (statements.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={7}>No statements.</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </div>
  );
}

function StatementRow({
  token,
  s,
  onDone,
}: {
  token: string;
  s: AdminStatement;
  onDone: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [payment, setPayment] = useState("");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);

  const call = async (path: string, body?: unknown) => {
    setBusy(true);
    setMsg(null);
    try {
      await adminApi(token, path, { method: "POST", body });
      onDone();
    } catch (err) {
      setMsg(err instanceof ApiError ? err.message : "failed");
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <tr>
        <td>{s.period}</td>
        <td className="mono">{s.organizationId.slice(0, 8)}…</td>
        <td>{s.status}</td>
        <td>{s.paymentStatus}</td>
        <td className="mono">{credits(s.totalMinor)} cr</td>
        <td className="mono">{credits(s.outstandingMinor)} cr</td>
        <td className="flex items-center gap-2">
          {s.status === "finalized" ? (
            <a href={`/api/v1/admin/statements/${s.id}/export.pdf`} className="text-primary">
              PDF
            </a>
          ) : null}
          <Button tone="ghost" onClick={() => setOpen((v) => !v)}>
            {open ? "Close" : "Ops"}
          </Button>
        </td>
      </tr>
      {open ? (
        <tr>
          <td colSpan={7} className="bg-canvas">
            <div className="flex flex-wrap items-end gap-2 py-1">
              {s.status === "draft" ? (
                <Button
                  tone="ghost"
                  disabled={busy}
                  onClick={() =>
                    call(`/statements/${s.id}/finalize`, {
                      idempotencyKey: `fin-${s.id}-${Date.now()}`,
                    })
                  }
                >
                  Finalize
                </Button>
              ) : null}
              {s.status === "finalized" && (s.paymentStatus === "pending" || s.paymentStatus === "overdue") ? (
                <>
                  <Field label="Payment (credits)">
                    <NumberInput
                      value={payment}
                      onChange={(e) => setPayment(e.target.value)}
                      min="0.000001"
                    />
                  </Field>
                  <Button
                    tone="ghost"
                    disabled={busy || payment === ""}
                    onClick={() =>
                      call(`/statements/${s.id}/payments`, {
                        amountMinor: Math.round(Number(payment) * 1_000_000),
                        idempotencyKey: `pay-${s.id}-${Date.now()}`,
                      })
                    }
                  >
                    Record payment
                  </Button>
                  <Button
                    tone="danger"
                    disabled={busy}
                    onClick={() =>
                      call(`/statements/${s.id}/suspend`, {
                        idempotencyKey: `sus-${s.id}-${Date.now()}`,
                      })
                    }
                  >
                    Suspend
                  </Button>
                </>
              ) : null}
              {s.status === "finalized" && s.paymentStatus === "suspended" ? (
                <span className="text-xs text-muted">payment suspended</span>
              ) : null}
              {msg ? <span className="text-xs text-danger">{msg}</span> : null}
            </div>
          </td>
        </tr>
      ) : null}
    </>
  );
}
