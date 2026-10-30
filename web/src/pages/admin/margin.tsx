import { Banner, EmptyRows, TableShell, Td, credits } from "../../components/ui";
import { useAdmin } from "./api";

export function Margin({ token }: { token: string }) {
  const margin = useAdmin<
    {
      channel: string;
      model: string;
      turns: number;
      chargedMinor: number;
      upstreamCostMinor: number;
      providerBilledMinor: number | null;
      marginMinor: number;
      untrackedTurns: number;
    }[]
  >(token, "/margin");
  const rows = margin.data ?? [];
  const totals = rows.reduce(
    (sum, row) => ({
      turns: sum.turns + row.turns,
      charged: sum.charged + row.chargedMinor,
      upstream: sum.upstream + row.upstreamCostMinor,
      margin: sum.margin + row.marginMinor,
      untracked: sum.untracked + row.untrackedTurns,
    }),
    { turns: 0, charged: 0, upstream: 0, margin: 0, untracked: 0 },
  );
  // A channel's provider bill repeats on every one of its model rows (the
  // figure is channel-scoped), so the total counts each channel once.
  const billedByChannel = new Map<string, number>();
  for (const row of rows) {
    if (row.providerBilledMinor !== null) {
      billedByChannel.set(row.channel, row.providerBilledMinor);
    }
  }
  const providerTotal = [...billedByChannel.values()].reduce((a, b) => a + b, 0);
  return (
    <div className="flex flex-col gap-3">
      {margin.error ? (
        <Banner tone="error">{margin.error}</Banner>
      ) : (
        <>
          <TableShell
            head={[
              "Channel",
              "Model",
              { label: "Turns", align: "right" },
              { label: "Charged", align: "right" },
              { label: "Upstream cost", align: "right" },
              { label: "Provider bill", align: "right" },
              { label: "Margin", align: "right" },
              { label: "Untracked", align: "right" },
            ]}
            density="compact"
            loading={margin.loading ? 6 : 0}
          >
            {!margin.loading &&
              rows.map((m) => (
              <tr key={`${m.channel}-${m.model}`}>
                <td>{m.channel}</td>
                <td>{m.model}</td>
                <Td align="right">{m.turns}</Td>
                <Td mono align="right">{credits(m.chargedMinor)} cr</Td>
                <Td mono align="right">{credits(m.upstreamCostMinor)} cr</Td>
                <Td mono align="right" className="text-xs">
                  {m.providerBilledMinor === null ? (
                    <span className="text-muted">not reconciled</span>
                  ) : (
                    `${credits(m.providerBilledMinor)} cr`
                  )}
                </Td>
                <Td mono align="right" className={m.marginMinor < 0 ? "text-danger" : "text-success"}>
                  {credits(m.marginMinor)} cr
                </Td>
                <Td align="right">{m.untrackedTurns}</Td>
              </tr>
            ))}
            {!margin.loading && rows.length > 0 ? (
              <tr className="font-semibold">
                <td>Total</td>
                <td>
                  {rows.length} row{rows.length === 1 ? "" : "s"}
                </td>
                <Td align="right">{totals.turns}</Td>
                <Td mono align="right">{credits(totals.charged)} cr</Td>
                <Td mono align="right">{credits(totals.upstream)} cr</Td>
                <Td mono align="right" className="text-xs">
                  {billedByChannel.size === 0 ? (
                    <span className="text-muted">no channel reconciled</span>
                  ) : (
                    <>
                      {credits(providerTotal)} cr{" "}
                      <span className="text-muted">
                        ({billedByChannel.size} channel{billedByChannel.size === 1 ? "" : "s"})
                      </span>
                    </>
                  )}
                </Td>
                <Td mono align="right" className={totals.margin < 0 ? "text-danger" : "text-success"}>
                  {credits(totals.margin)} cr
                </Td>
                <Td align="right">{totals.untracked}</Td>
              </tr>
            ) : !margin.loading ? (
              <EmptyRows colSpan={8}>
                No settled turns yet — a row appears once a gateway turn settles and its upstream
                cost is recorded.
              </EmptyRows>
            ) : null}
          </TableShell>
          <p className="text-xs text-muted">
            The provider bill is what the upstream billed the channel, summed per day by the
            upstream-bill-sync pass; “not reconciled” means that pass has not billed the channel
            yet, not that it billed nothing. The total counts each channel once — the API repeats
            the channel-scoped figure on every model row. Untracked turns are settled turns with no
            upstream cost recorded (an untracked price, a swept hold, or history), so a growing gap
            there is where missing upstream pricing shows up first.
          </p>
        </>
      )}
    </div>
  );
}
