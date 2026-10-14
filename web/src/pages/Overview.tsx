import { Link } from "react-router-dom";
import { useBalance, useBillingRecords, useSession } from "../api/hooks";
import { Card, EmptyRows, Kpi, PageState, TableShell, credits } from "../components/ui";

export function Overview() {
  const session = useSession();
  const balance = useBalance();
  const records = useBillingRecords(5);

  return (
    <div className="flex flex-col gap-6">
      <h1 className="text-2xl font-bold">Overview</h1>
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
        <Kpi
          label="Available balance"
          value={balance.data ? `${credits(balance.data.availableMinor)} cr` : "…"}
        />
        <Kpi
          label="Credit limit"
          value={balance.data ? `${credits(balance.data.creditLimitMinor)} cr` : "…"}
        />
        <Kpi
          label="Credit drawn"
          value={balance.data ? `${credits(balance.data.creditUsedMinor)} cr` : "…"}
        />
      </div>
      <Card title="Recent bills">
        {records.isLoading ? (
          <PageState>Loading…</PageState>
        ) : records.isError ? (
          <PageState>The billing records could not be read.</PageState>
        ) : (
          <TableShell head={["Booked", "Model", "Kind", "Charged", ""]}>
            {(records.data?.rows ?? []).map((row) => (
              <tr key={row.requestId}>
                <td>{row.bookedOn}</td>
                <td>{row.model}</td>
                <td>{row.kind}</td>
                <td className="num mono">{credits(row.chargedMinor)}</td>
                <td>
                  <a href={`/verify?entry=${row.entryId}`} className="text-primary">
                    verify
                  </a>
                </td>
              </tr>
            ))}
            {(records.data?.rows.length ?? 0) === 0 ? (
              <EmptyRows colSpan={5}>No billing records yet.</EmptyRows>
            ) : null}
          </TableShell>
        )}
        <p className="mt-3 text-sm">
          <Link to="/bills" className="text-primary">
            All bills →
          </Link>
        </p>
      </Card>
      <p className="text-xs text-muted">
        Signed in as {session.data?.user.email} — this is the React dashboard scaffold (issue
        #234); the Topcoat pages under <a className="text-primary" href="/dashboard">/dashboard</a>{" "}
        stay authoritative until parity.
      </p>
    </div>
  );
}
