import { useKeys } from "../api/hooks";
import { EmptyRows, PageState, TableShell } from "../components/ui";

export function Keys() {
  const keys = useKeys();
  const list = Array.isArray(keys.data) ? keys.data : (keys.data?.keys ?? []);
  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">API keys</h1>
      </div>
      {keys.isLoading ? (
        <PageState>Loading…</PageState>
      ) : keys.isError ? (
        <PageState>The keys could not be read.</PageState>
      ) : (
        <TableShell head={["Label", "Prefix", "Budget", "Spend limit", "Created", ""]}>
          {list.map((key) => (
            <tr key={key.id}>
              <td>{key.label}</td>
              <td className="mono">{key.prefix}…</td>
              <td>{key.budgetDuration ?? "—"}</td>
              <td className="num">{key.spendLimitMinor ?? "—"}</td>
              <td>{key.createdAt.slice(0, 10)}</td>
              <td>{key.revokedAt ? <span className="text-danger">revoked</span> : null}</td>
            </tr>
          ))}
          {list.length === 0 ? <EmptyRows colSpan={6}>No keys yet.</EmptyRows> : null}
        </TableShell>
      )}
    </div>
  );
}
