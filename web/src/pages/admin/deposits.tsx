import { Fragment, useState } from "react";
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
import { adminApi, useAdmin } from "./api";
import type { AdminOrg } from "./organizations";

/// One money-in row the deposits view lists (issue #354); the reversal fields
/// land once `reversed` (issue #356).
type AdminDeposit = {
  depositId: string;
  organizationId: string;
  organization: string;
  rail: string;
  paymentRef: string;
  amountMinor: number;
  receivedMinor: number | null;
  status: string;
  entryId: string | null;
  reversalEntryId: string | null;
  reversedAt: string | null;
  refundStatus: "pending" | "succeeded" | "failed" | null;
  disputedAt: string | null;
  createdAt: string;
};

/// The deposits view: the money-in record, newest first, filterable by one
/// organization. A `credited` row carries a **Reverse** action — a refund
/// (issues #356, #360): it posts one entry taking the purchased credit back,
/// flips the row to `reversed`, and for a rail that moves money back it also
/// issues the rail's refund, whose standing the row carries as `refundStatus`.
export function Deposits({ token }: { token: string }) {
  const [organization, setOrganization] = useState("");
  const [reversing, setReversing] = useState<string | null>(null);
  const orgs = useAdmin<
    { organizations?: AdminOrg[]; orgs?: AdminOrg[]; nextCursor?: string } | AdminOrg[]
  >(token, "/organizations", []);
  const orgRows = Array.isArray(orgs.data)
    ? orgs.data
    : (orgs.data?.organizations ?? orgs.data?.orgs ?? []);
  const deposits = useAdmin<AdminDeposit[]>(
    token,
    `/deposits?limit=100${organization ? `&organizationId=${organization}` : ""}`,
    [organization],
  );
  const rows = deposits.data ?? [];
  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <label className="text-xs text-muted" htmlFor="deposit-org">
          Organization
        </label>
        <select
          id="deposit-org"
          className="rounded-md border border-line bg-surface px-2 py-1 text-sm"
          value={organization}
          onChange={(e) => setOrganization(e.target.value)}
        >
          <option value="">All organizations</option>
          {orgRows.map((o) => (
            <option key={o.id} value={o.id}>
              {o.name}
            </option>
          ))}
        </select>
      </div>
      {deposits.error ? (
        <Banner tone="error">{deposits.error}</Banner>
      ) : (
        <TableShell
          head={[
            "When",
            "Organization",
            "Rail",
            "Status",
            { label: "Amount", align: "right" },
            { label: "Received", align: "right" },
            { label: "Reference", truncate: true },
            "Entry",
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
                <td>{d.rail}</td>
                <td className="text-xs">
                  <span
                    className={
                      d.status === "credited"
                        ? "text-success"
                        : d.status === "reversed"
                          ? "text-danger"
                          : "text-muted"
                    }
                  >
                    {d.status}
                  </span>
                  {d.refundStatus ? (
                    <span
                      className={`block ${
                        d.refundStatus === "succeeded"
                          ? "text-success"
                          : d.refundStatus === "failed"
                            ? "text-danger"
                            : "text-muted"
                      }`}
                    >
                      refund {d.refundStatus}
                    </span>
                  ) : null}
                  {d.disputedAt ? (
                    <span className="block text-danger">disputed</span>
                  ) : null}
                </td>
                <Td mono align="right">{credits(d.amountMinor)} cr</Td>
                <Td mono align="right">
                  {d.receivedMinor === null ? "—" : `${credits(d.receivedMinor)} cr`}
                </Td>
                <Td mono truncate className="text-xs text-muted">{d.paymentRef}</Td>
                <td className="mono text-xs text-muted">
                  {d.status === "reversed" && d.reversalEntryId
                    ? `↩ ${d.reversalEntryId.slice(0, 8)}…`
                    : d.entryId
                      ? `${d.entryId.slice(0, 8)}…`
                      : "—"}
                </td>
                <td>
                  {d.status === "credited" ? (
                    <Button
                      tone="ghost"
                      onClick={() =>
                        setReversing(reversing === d.depositId ? null : d.depositId)
                      }
                    >
                      Reverse
                    </Button>
                  ) : null}
                </td>
              </tr>
              {reversing === d.depositId ? (
                <tr>
                  <td colSpan={9}>
                    <ReverseDeposit
                      token={token}
                      deposit={d}
                      onDone={() => {
                        setReversing(null);
                        deposits.refresh();
                      }}
                    />
                  </td>
                </tr>
              ) : null}
            </Fragment>
            ))}
          {!deposits.loading && rows.length === 0 ? (
            <EmptyRows colSpan={9}>
              No deposits yet — a top-up, a code redemption or a rail payment appears here.
            </EmptyRows>
          ) : null}
        </TableShell>
      )}
      <p className="text-xs text-muted">
        Statuses are the rails' facts: pending (opened, not paid), confirmed (the rail
        says paid, the ledger has not credited yet), credited (the entry named on the
        row moved the wallet), reversed (a refund or chargeback — the Entry cell then
        names the reversal's own entry) or expired. A received amount appears only
        when the rail reported a different figure than the deposit was opened for.
        Reversing posts one ledger entry taking the purchased credit back — and
        for a rail that moves money back (the Hyperswitch switch) it issues the
        rail's refund too: the row then shows `refund pending`/`succeeded`/`failed`
        under the status, `failed` meaning the rail refused the call so a retry
        re-issues it. `disputed` marks a charge the rail reported contested.
      </p>
    </div>
  );
}

/// The inline reverse form under a credited deposit's row (issue #356): a
/// required reason — it becomes the reversal entry's description — then one
/// call that debits the wallet and credits cash. A refusal surfaces inline:
/// `402` when the customer already spent the credit, `409` when the row moved
/// on between listing and the click.
function ReverseDeposit({
  token,
  deposit,
  onDone,
}: {
  token: string;
  deposit: AdminDeposit;
  onDone: () => void;
}) {
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  return (
    <form
      className="my-1 flex flex-wrap items-end gap-2"
      onSubmit={async (e) => {
        e.preventDefault();
        setBusy(true);
        setMsg(null);
        try {
          await adminApi(token, `/deposits/${deposit.depositId}/reverse`, {
            method: "POST",
            body: { reason },
          });
          onDone();
        } catch (err) {
          setMsg(err instanceof ApiError ? err.message : "failed");
          setBusy(false);
        }
      }}
    >
      <Field
        label={`Reverse ${credits(deposit.amountMinor)} cr — reason`}
        hint="required — it becomes the reversal entry's description"
      >
        <TextInput
          value={reason}
          maxLength={200}
          onChange={(e) => setReason(e.target.value)}
        />
      </Field>
      <Button type="submit" disabled={busy || reason.trim() === ""}>
        Reverse deposit
      </Button>
      {msg ? <span className="text-xs text-danger">{msg}</span> : null}
    </form>
  );
}
