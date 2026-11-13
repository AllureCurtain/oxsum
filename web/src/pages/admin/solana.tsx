import { Fragment, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Banner,
  Button,
  EmptyRows,
  Field,
  TableShell,
  Td,
  TextInput,
  credits,
} from "../../components/ui";
import { ApiError } from "../../api/client";
import type { SolanaExceptionDeposit } from "../../api/hooks";
import { adminApi, useAdmin } from "./api";

/// The exception statuses the queue filters by — the rail's own vocabulary,
/// `all` being the unfiltered view.
const STATUSES = ["underpaid", "overpaid", "late", "flagged", "refund_pending", "refunded"] as const;

/// The tone a status badge reads with: the queue separates "awaits the
/// customer" (refund pending) from "done" (refunded) from "needs a decision".
function statusTone(status: string): string {
  switch (status) {
    case "refunded":
      return "text-success";
    case "refund_pending":
      return "text-pending";
    default:
      return "text-danger";
  }
}

/// Whether the row's refund can still move forward: `refund_pending` is the
/// only state the lifecycle accepts, and approval needs the customer's
/// address first.
function refundable(status: string): boolean {
  return status === "underpaid" || status === "overpaid" || status === "late" || status === "flagged";
}

/**
 * The solana rail's exception queue (issue #1142): deposits whose located
 * paying transfer could not settle — underpaid, overpaid, late or flagged —
 * plus the refund lifecycle's own states, filterable by status. The `asset`
 * column names the whitelist key the intent locked — the denomination
 * `delivered`/`expected` count in and the refund repays. The refund is a
 * pull payment: the operator opens it, the customer names the refund
 * address on their own surface, the operator approves, the multisig pays
 * out-of-band, and the recorded signature closes the row. The actions run
 * inline under each row: open, approve (once the address is in), complete
 * (with the payout signature).
 */
export function SolanaExceptions({ token }: { token: string }) {
  const { t } = useTranslation();
  const [status, setStatus] = useState("");
  const [acting, setActing] = useState<string | null>(null);
  const deposits = useAdmin<{ entries: SolanaExceptionDeposit[]; nextCursor?: string }>(
    token,
    `/solana/exceptions?limit=100${status ? `&status=${status}` : ""}`,
    [status],
  );
  const rows = deposits.data?.entries ?? [];
  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <label className="text-xs text-muted" htmlFor="solana-ex-status">
          {t("admin.solanaExceptions.status")}
        </label>
        <select
          id="solana-ex-status"
          className="rounded-md border border-line bg-surface px-2 py-1 text-sm"
          value={status}
          onChange={(e) => setStatus(e.target.value)}
        >
          <option value="">{t("admin.solanaExceptions.allStatuses")}</option>
          {STATUSES.map((s) => (
            <option key={s} value={s}>
              {t(`admin.solanaExceptions.statuses.${s}`)}
            </option>
          ))}
        </select>
      </div>
      {deposits.error ? (
        <Banner tone="error">{deposits.error}</Banner>
      ) : (
        <TableShell
          head={[
            t("admin.solanaExceptions.colWhen"),
            t("admin.solanaExceptions.colOrganization"),
            t("admin.solanaExceptions.colStatus"),
            t("admin.solanaExceptions.colAsset"),
            { label: t("admin.solanaExceptions.colOpened"), align: "right" },
            { label: t("admin.solanaExceptions.colDelivered"), align: "right" },
            { label: t("admin.solanaExceptions.colSignature"), truncate: true },
            t("admin.solanaExceptions.colRefund"),
            "",
          ]}
          density="compact"
          loading={deposits.loading ? 6 : 0}
        >
          {!deposits.loading &&
            rows.map((d) => (
              <Fragment key={d.depositId}>
                <tr>
                  <td className="mono text-xs">{d.createdAt.slice(0, 19).replace("T", " ")}</td>
                  <td>{d.organization}</td>
                  <td className={`text-xs ${statusTone(d.status)}`}>
                    {t(`admin.solanaExceptions.statuses.${d.status}`, { defaultValue: d.status })}
                    {d.status === "refund_pending" && !d.refundAddress ? (
                      <span className="block text-muted">{t("admin.solanaExceptions.awaitingAddress")}</span>
                    ) : null}
                    {d.status === "refund_pending" && d.refundAddress && !d.refund?.approvedAt ? (
                      <span className="block text-muted">{t("admin.solanaExceptions.awaitingApproval")}</span>
                    ) : null}
                    {d.status === "refund_pending" && d.refund?.approvedAt ? (
                      <span className="block text-muted">{t("admin.solanaExceptions.awaitingPayout")}</span>
                    ) : null}
                  </td>
                  <td className="mono text-xs">{d.asset ?? "—"}</td>
                  <Td mono align="right">{credits(d.amountMinor)} cr</Td>
                  <Td mono align="right">
                    {d.receivedMinor === null ? "—" : `${credits(d.receivedMinor)} cr`}
                  </Td>
                  <Td mono truncate className="text-xs text-muted">
                    {d.signature ?? "—"}
                  </Td>
                  <td className="mono text-xs text-muted">
                    {d.refundSignature ? `${d.refundSignature.slice(0, 12)}…` : d.refund?.reason ?? "—"}
                  </td>
                  <td>
                    {refundable(d.status) ||
                    (d.status === "refund_pending" && (d.refundAddress || d.refund?.approvedAt)) ? (
                      <Button
                        tone="ghost"
                        onClick={() => setActing(acting === d.depositId ? null : d.depositId)}
                      >
                        {t("admin.solanaExceptions.review")}
                      </Button>
                    ) : null}
                  </td>
                </tr>
                {acting === d.depositId ? (
                  <tr>
                    <td colSpan={9}>
                      <RefundActions
                        token={token}
                        deposit={d}
                        onDone={() => {
                          setActing(null);
                          deposits.refresh();
                        }}
                      />
                    </td>
                  </tr>
                ) : null}
              </Fragment>
            ))}
          {!deposits.loading && rows.length === 0 ? (
            <EmptyRows colSpan={9}>{t("admin.solanaExceptions.empty")}</EmptyRows>
          ) : null}
        </TableShell>
      )}
      <p className="text-xs text-muted">{t("admin.solanaExceptions.note")}</p>
    </div>
  );
}

/// The inline refund-lifecycle actions under a queue row (issue #1142):
/// which forms show depends on where the deposit's lifecycle stands —
/// `refundable` rows get the open form; a `refund_pending` one gets the
/// approve button once the address is in, then the complete form once
/// approved. Refusals surface inline: `409` when the row moved on between
/// the listing and the click.
function RefundActions({
  token,
  deposit,
  onDone,
}: {
  token: string;
  deposit: SolanaExceptionDeposit;
  onDone: () => void;
}) {
  const { t } = useTranslation();
  const [reason, setReason] = useState("");
  const [signature, setSignature] = useState("");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);

  const run = async (path: string, body?: unknown) => {
    setBusy(true);
    setMsg(null);
    try {
      await adminApi(token, path, { method: "POST", body });
      onDone();
    } catch (err) {
      setMsg(err instanceof ApiError ? err.message : "failed");
      setBusy(false);
    }
  };

  return (
    <div className="my-1 flex flex-col gap-2">
      {deposit.refundAddress ? (
        <p className="text-xs text-muted">
          {t("admin.solanaExceptions.refundAddress")}{" "}
          <code className="mono">{deposit.refundAddress}</code>
        </p>
      ) : null}
      {refundable(deposit.status) ? (
        <form
          className="flex flex-wrap items-end gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            void run(`/solana/${deposit.depositId}/refund`, { reason });
          }}
        >
          <Field
            label={t("admin.solanaExceptions.refundLabel", { amount: credits(deposit.amountMinor) })}
            hint={t("admin.solanaExceptions.refundHint")}
          >
            <TextInput value={reason} maxLength={200} onChange={(e) => setReason(e.target.value)} />
          </Field>
          <Button type="submit" disabled={busy || reason.trim() === ""}>
            {t("admin.solanaExceptions.openRefund")}
          </Button>
        </form>
      ) : null}
      {deposit.status === "refund_pending" && deposit.refundAddress && !deposit.refund?.approvedAt ? (
        <div>
          <Button
            tone="ghost"
            disabled={busy}
            onClick={() => void run(`/solana/${deposit.depositId}/refund-approve`)}
          >
            {t("admin.solanaExceptions.approveRefund")}
          </Button>
        </div>
      ) : null}
      {deposit.status === "refund_pending" && deposit.refund?.approvedAt ? (
        <form
          className="flex flex-wrap items-end gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            void run(`/solana/${deposit.depositId}/refund-complete`, { signature });
          }}
        >
          <Field
            label={t("admin.solanaExceptions.completeLabel")}
            hint={t("admin.solanaExceptions.completeHint")}
          >
            <TextInput
              value={signature}
              className="mono"
              maxLength={120}
              onChange={(e) => setSignature(e.target.value)}
            />
          </Field>
          <Button type="submit" disabled={busy || signature.trim() === ""}>
            {t("admin.solanaExceptions.completeRefund")}
          </Button>
        </form>
      ) : null}
      {msg ? <span className="text-xs text-danger">{msg}</span> : null}
    </div>
  );
}
