import { useMemo, useState } from "react";
import { useDisplay, useKeys, useUsage } from "../api/hooks";
import { Cr, EmptyRows, PageState, Select, TableShell, Td, creditsWithDisplay } from "../components/ui";

// The usage page: the trailing 30 UTC days, summed per day for the chart and
// listed per (day, channel, model) slice in the table — the same window and
// scope the Topcoat page draws.
const USAGE_DAYS = 30;

function dayString(offset: number): string {
  const d = new Date();
  d.setUTCDate(d.getUTCDate() - offset);
  return d.toISOString().slice(0, 10);
}

export function Usage() {
  const [keyFilter, setKeyFilter] = useState("");
  const display = useDisplay();
  const keys = useKeys();
  const usage = useUsage(dayString(USAGE_DAYS - 1), dayString(0));

  const rows = useMemo(
    () =>
      (usage.data?.days ?? []).filter(
        (row) => !keyFilter || row.keyId === keyFilter,
      ),
    [usage.data, keyFilter],
  );

  const perDay = useMemo(() => {
    const map = new Map<string, number>();
    for (let i = USAGE_DAYS - 1; i >= 0; i--) {
      map.set(dayString(i), 0);
    }
    for (const row of rows) {
      map.set(row.day, (map.get(row.day) ?? 0) + row.chargedMinor);
    }
    return map;
  }, [rows]);

  const max = Math.max(1, ...perDay.values());

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Usage</h1>
        <Select value={keyFilter} onChange={(e) => setKeyFilter(e.target.value)} aria-label="key filter">
          <option value="">all keys</option>
          {(keys.data ?? []).map((key) => (
            <option key={key.id} value={key.id}>
              {key.name ?? key.prefix}
            </option>
          ))}
        </Select>
      </div>

      {/* The daily chart: one bar per UTC day, charged amount. Pure CSS — no
          chart dependency for a single series. */}
      <div className="rounded-md border border-line bg-surface p-4">
        <div className="flex h-32 items-end gap-1">
          {[...perDay.entries()].map(([day, charged]) => (
            <div
              key={day}
              className="group relative flex-1 rounded-t bg-primary/70 hover:bg-primary"
              style={{ height: `${Math.max(2, (charged / max) * 100)}%` }}
              title={`${day}: ${creditsWithDisplay(charged, display)}`}
            >
              <span className="pointer-events-none absolute -top-8 left-1/2 hidden -translate-x-1/2 whitespace-nowrap rounded bg-ink px-2 py-1 text-xs text-white group-hover:block">
                {day}: {creditsWithDisplay(charged, display)}
              </span>
            </div>
          ))}
        </div>
      </div>

      {usage.isError ? (
        <PageState>The usage could not be read.</PageState>
      ) : (
        <TableShell
          head={[
            "Day",
            "Channel",
            "Model",
            "Key",
            { label: "Turns", align: "right" },
            { label: "In", align: "right" },
            { label: "Out", align: "right" },
            { label: "Cached", align: "right" },
            { label: "Reasoning", align: "right" },
            { label: "Charged", align: "right" },
          ]}
          loading={usage.isLoading ? 8 : 0}
        >
          {!usage.isLoading &&
            rows.map((row, i) => (
              <tr key={`${row.day}-${row.channel}-${row.model}-${row.keyId ?? "shared"}-${i}`}>
                <td>{row.day}</td>
                <td>{row.channel}</td>
                <td>{row.model}</td>
                <td className="mono">{keyLabel(row.keyId, keys.data)}</td>
                <Td mono align="right">{row.turns}</Td>
                <Td mono align="right">{row.inputTokens}</Td>
                <Td mono align="right">{row.outputTokens}</Td>
                <Td mono align="right">{row.cachedTokens}</Td>
                <Td mono align="right">{row.reasoningTokens}</Td>
                <Td mono align="right"><Cr minor={row.chargedMinor} currency={display} /></Td>
              </tr>
            ))}
          {!usage.isLoading && rows.length === 0 ? (
            <EmptyRows colSpan={10}>No usage in this window.</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </div>
  );
}

function keyLabel(keyId: string | null | undefined, keys?: { id: string; name?: string | null; prefix: string }[]) {
  if (!keyId) {
    return "shared";
  }
  const key = keys?.find((k) => k.id === keyId || k.id.replaceAll("-", "") === keyId);
  return key ? (key.name ?? key.prefix) : keyId.slice(0, 8);
}
