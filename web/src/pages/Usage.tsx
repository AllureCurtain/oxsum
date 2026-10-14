import { useUsage } from "../api/hooks";
import { EmptyRows, PageState, TableShell, credits } from "../components/ui";

function daysAgo(n: number): string {
  const d = new Date(Date.now() - n * 86_400_000);
  return d.toISOString().slice(0, 10);
}

export function Usage() {
  const usage = useUsage(daysAgo(31), daysAgo(0));
  const days = usage.data?.days ?? [];
  return (
    <div className="flex flex-col gap-4">
      <h1 className="text-2xl font-bold">Usage</h1>
      <p className="text-sm text-muted">Daily rollup over the last 31 days.</p>
      {usage.isLoading ? (
        <PageState>Loading…</PageState>
      ) : usage.isError ? (
        <PageState>The usage rollup could not be read.</PageState>
      ) : (
        <TableShell
          head={["Day", "Channel", "Model", "Turns", "Input", "Output", "Cached", "Reasoning", "Charged"]}
        >
          {days.map((d) => (
            <tr key={`${d.day}-${d.channel}-${d.model}-${d.keyId ?? ""}`}>
              <td>{d.day}</td>
              <td>{d.channel}</td>
              <td>{d.model}</td>
              <td className="num">{d.turns}</td>
              <td className="num">{d.inputTokens}</td>
              <td className="num">{d.outputTokens}</td>
              <td className="num">{d.cachedTokens}</td>
              <td className="num">{d.reasoningTokens}</td>
              <td className="num mono">{credits(d.chargedMinor)}</td>
            </tr>
          ))}
          {days.length === 0 ? <EmptyRows colSpan={9}>No usage in this window.</EmptyRows> : null}
        </TableShell>
      )}
    </div>
  );
}
