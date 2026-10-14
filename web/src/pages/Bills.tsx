import { useBillingRecords } from "../api/hooks";
import { EmptyRows, PageState, TableShell, credits } from "../components/ui";

export function Bills() {
  const records = useBillingRecords(50);
  return (
    <div className="flex flex-col gap-4">
      <h1 className="text-2xl font-bold">Bills</h1>
      {records.isLoading ? (
        <PageState>Loading…</PageState>
      ) : records.isError ? (
        <PageState>The billing records could not be read.</PageState>
      ) : (
        <TableShell
          head={["Booked", "Request", "Model", "Kind", "Status", "In/Out tokens", "Charged", ""]}
        >
          {(records.data?.rows ?? []).map((row) => (
            <tr key={row.requestId}>
              <td>{row.bookedOn}</td>
              <td className="mono">{row.requestId.slice(0, 8)}</td>
              <td>{row.model}</td>
              <td>{row.kind}</td>
              <td>{row.status === "settled" ? row.status : `${row.status} — ${row.failureReason ?? ""}`}</td>
              <td className="num">
                {row.inputTokens}/{row.outputTokens}
              </td>
              <td className="num mono">{credits(row.chargedMinor)}</td>
              <td>
                <a href={`/verify?entry=${row.entryId}`} className="text-primary">
                  verify
                </a>
              </td>
            </tr>
          ))}
          {(records.data?.rows.length ?? 0) === 0 ? (
            <EmptyRows colSpan={8}>No billing records yet.</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </div>
  );
}
