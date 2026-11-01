import { useTranslation } from "react-i18next";
import { Banner, EmptyRows, TableShell, Td, credits } from "../../components/ui";
import { useAdmin } from "./api";

export function Margin({ token }: { token: string }) {
  const { t } = useTranslation();
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
              t("admin.margin.colChannel"),
              t("admin.margin.colModel"),
              { label: t("admin.margin.colTurns"), align: "right" },
              { label: t("admin.margin.colCharged"), align: "right" },
              { label: t("admin.margin.colUpstreamCost"), align: "right" },
              { label: t("admin.margin.colProviderBill"), align: "right" },
              { label: t("admin.margin.colMargin"), align: "right" },
              { label: t("admin.margin.colUntracked"), align: "right" },
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
                    <span className="text-muted">{t("admin.margin.notReconciled")}</span>
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
                <td>{t("admin.margin.total")}</td>
                <td>
                  {t("admin.margin.rowCount", { count: rows.length })}
                </td>
                <Td align="right">{totals.turns}</Td>
                <Td mono align="right">{credits(totals.charged)} cr</Td>
                <Td mono align="right">{credits(totals.upstream)} cr</Td>
                <Td mono align="right" className="text-xs">
                  {billedByChannel.size === 0 ? (
                    <span className="text-muted">{t("admin.margin.noChannelReconciled")}</span>
                  ) : (
                    <>
                      {credits(providerTotal)} cr{" "}
                      <span className="text-muted">
                        ({t("admin.margin.channelCount", { count: billedByChannel.size })})
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
                {t("admin.margin.empty")}
              </EmptyRows>
            ) : null}
          </TableShell>
          <p className="text-xs text-muted">
            {t("admin.margin.note")}
          </p>
        </>
      )}
    </div>
  );
}
