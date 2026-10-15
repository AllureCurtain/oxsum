import { Link } from "react-router-dom";
import { useBalance, useBudget, useEntries, useHolds, useSession } from "../api/hooks";
import { Card, EmptyRows, Kpi, PageState, TableShell, credits } from "../components/ui";

export function Overview() {
  const session = useSession();
  const balance = useBalance();
  const budget = useBudget();
  const holds = useHolds();
  const entries = useEntries(10);

  return (
    <div className="flex flex-col gap-6">
      <h1 className="text-2xl font-bold">Overview</h1>
      {!session.data?.user.emailVerified ? (
        <p className="rounded-md border border-pending/40 bg-amber-50 px-3 py-2 text-sm text-pending">
          Your email is not verified. Check your inbox for the verification mail.
        </p>
      ) : null}
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <Kpi
          label="Available"
          value={balance.data ? `${credits(balance.data.availableMinor)} cr` : "…"}
        />
        <Kpi
          label="Frozen"
          value={balance.data ? `${credits(balance.data.reservedMinor)} cr` : "…"}
          hint="what in-flight holds reserve"
        />
        <Kpi
          label="Spent this month"
          value={balance.data ? `${credits(balance.data.spentThisMonthMinor)} cr` : "…"}
        />
        <Kpi
          label="Credit line"
          value={
            balance.data
              ? `${credits(balance.data.creditUsedMinor)} / ${credits(balance.data.creditLimitMinor)} cr`
              : "…"
          }
          hint={
            budget.data?.organization.consumedPercent != null
              ? `${budget.data.organization.consumedPercent}% of the line committed`
              : "no credit line"
          }
        />
      </div>

      {budget.data?.member ? (
        <Card title="Your member budget">
          <p className="text-sm">
            {budget.data.member.consumedPercent ?? 0}% of{" "}
            {credits(budget.data.member.budgetLimitMinor)} cr committed this window.
          </p>
        </Card>
      ) : null}

      {budget.data && budget.data.keys.some((k) => k.spendLimitMinor != null) ? (
        <Card title="Key budgets">
          <TableShell head={["Key", "Window", "Committed", "Consumed"]}>
            {budget.data.keys
              .filter((k) => k.spendLimitMinor != null)
              .map((k) => (
                <tr key={k.keyId}>
                  <td>
                    {k.name ?? <code className="mono">{k.prefix}</code>}
                  </td>
                  <td>{k.budgetDuration ?? "cumulative"}</td>
                  <td className="mono">{credits(k.committedMinor)} cr</td>
                  <td>{k.consumedPercent != null ? `${k.consumedPercent}%` : "—"}</td>
                </tr>
              ))}
          </TableShell>
        </Card>
      ) : null}

      <Card
        title="In-flight holds"
        actions={
          <Link to="/requests" className="text-sm text-primary">
            Requests →
          </Link>
        }
      >
        {holds.isLoading ? (
          <PageState>Loading…</PageState>
        ) : (
          <TableShell head={["Request", "Model", "Key", "Frozen"]}>
            {(holds.data?.holds ?? []).map((hold) => (
              <tr key={hold.requestId}>
                <td className="mono">{hold.requestId}</td>
                <td>{hold.model}</td>
                <td className="mono">{hold.keyPrefix ?? "—"}</td>
                <td className="mono">{credits(hold.freezeMinor)} cr</td>
              </tr>
            ))}
            {(holds.data?.holds.length ?? 0) === 0 ? (
              <EmptyRows colSpan={4}>Nothing in flight.</EmptyRows>
            ) : null}
          </TableShell>
        )}
      </Card>

      <Card
        title="Recent entries"
        actions={
          <Link to="/log" className="text-sm text-primary">
            Full log →
          </Link>
        }
      >
        {entries.isLoading ? (
          <PageState>Loading…</PageState>
        ) : (
          <TableShell head={["#", "Description"]}>
            {(entries.data?.rows ?? []).map((entry) => (
              <tr key={entry.id}>
                <td className="mono">{entry.index}</td>
                <td className="mono">{entry.description}</td>
              </tr>
            ))}
            {(entries.data?.rows.length ?? 0) === 0 ? (
              <EmptyRows colSpan={2}>No entries yet.</EmptyRows>
            ) : null}
          </TableShell>
        )}
      </Card>
    </div>
  );
}
