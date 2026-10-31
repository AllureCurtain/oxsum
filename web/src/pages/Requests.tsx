import { useMemo, useState } from "react";
import { ArrowLeftRight } from "lucide-react";
import { useSearchParams } from "react-router-dom";
import { settlementOutcome } from "../lib/kinds";
import { useBillingRecords, useDisplay, useKeys } from "../api/hooks";
import { useBillingSocket } from "../api/ws";
import {
  EmptyRows,
  PageState,
  StatChip,
  StatusBadge,
  TableShell,
  Td,
  TextInput,
  Button,
  Cr,
} from "../components/ui";

// The requests page: the newest gateway requests with the live socket's
// in-flight turns on top. Filters ride the URL (`?key=&model=`) so a filtered
// view is a link that survives a reload, same as the Topcoat page.
export function Requests() {
  const [params, setParams] = useSearchParams();
  const [before, setBefore] = useState<number | undefined>(undefined);
  const records = useBillingRecords(100, before);
  const keys = useKeys();
  const display = useDisplay();
  const socket = useBillingSocket();

  const keyFilter = params.get("key") ?? "";
  const modelFilter = params.get("model") ?? "";

  const prefixOf = useMemo(() => {
    const map = new Map<string, string>();
    for (const key of keys.data ?? []) {
      map.set(key.id.replaceAll("-", ""), key.prefix);
      map.set(key.id, key.prefix);
    }
    return map;
  }, [keys.data]);

  const rows = useMemo(
    () =>
      (records.data?.rows ?? []).filter((row) => {
        const prefix = row.keyId ? prefixOf.get(row.keyId) : undefined;
        if (keyFilter && prefix !== keyFilter) {
          return false;
        }
        if (modelFilter && row.model !== modelFilter) {
          return false;
        }
        return true;
      }),
    [records.data, keyFilter, modelFilter, prefixOf],
  );

  // The band over the filters: the table answers "which requests" and these
  // answer "how much of today is that". Same loaded window, client-side sums.
  const stats = useMemo(() => {
    const today = new Date().toISOString().slice(0, 10);
    let todayTurns = 0;
    let todayTokens = 0;
    let todayCharged = 0;
    for (const row of records.data?.rows ?? []) {
      if (row.bookedOn === today) {
        todayTurns += 1;
        todayTokens += row.inputTokens + row.outputTokens;
        todayCharged += row.chargedMinor;
      }
    }
    const inFlight = socket.turns.filter((t) => t.state !== "settled").length;
    return { todayTurns, todayTokens, todayCharged, inFlight };
  }, [records.data, socket.turns]);

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Requests</h1>
        {socket.dead ? (
          <button
            type="button"
            onClick={socket.reconnect}
            title="Reconnect the billing socket"
            className="text-xs font-medium text-danger hover:text-ink"
          >
            connection lost — retry
          </button>
        ) : (
          <span className={`text-xs ${socket.live ? "text-success" : "text-muted"}`}>
            {socket.live ? "live" : "offline"}
          </span>
        )}
      </div>

      <div className="flex flex-wrap gap-x-10 gap-y-4 rounded-xl border border-line bg-surface px-5 py-4 shadow-[0_1px_3px_rgba(20,34,64,.05)]">
        <StatChip
          label="In flight"
          value={stats.inFlight}
          hint={stats.inFlight > 0 ? "turns live" : "none"}
        />
        <StatChip label="Turns today" value={stats.todayTurns} />
        <StatChip
          label="Tokens today"
          value={stats.todayTokens.toLocaleString("en-US")}
          hint="in + out"
        />
        <StatChip
          label="Charged today"
          value={<Cr minor={stats.todayCharged} currency={display} />}
        />
        <StatChip
          label="Avg tokens/turn"
          value={
            stats.todayTurns > 0 ? Math.round(stats.todayTokens / stats.todayTurns).toLocaleString("en-US") : "—"
          }
        />
      </div>

      <div className="flex flex-wrap gap-3">
        <TextInput
          placeholder="Key prefix"
          value={keyFilter}
          onChange={(e) =>
            setParams((p) => {
              const next = new URLSearchParams(p);
              e.target.value ? next.set("key", e.target.value) : next.delete("key");
              return next;
            })
          }
        />
        <TextInput
          placeholder="Model"
          value={modelFilter}
          onChange={(e) =>
            setParams((p) => {
              const next = new URLSearchParams(p);
              e.target.value ? next.set("model", e.target.value) : next.delete("model");
              return next;
            })
          }
        />
        {keyFilter || modelFilter ? (
          <Button tone="ghost" onClick={() => setParams({})}>
            Clear filters
          </Button>
        ) : null}
      </div>

      {socket.turns.length > 0 ? (
        <TableShell
          head={[{ label: "Request", truncate: true }, "Model", "State", { label: "Live", align: "right" }]}
          sticky={false}
        >
          {socket.turns.map((turn) => (
            <tr key={turn.requestId}>
              <Td mono truncate>{turn.requestId}</Td>
              <td>{turn.model}</td>
              <td>
                <StatusBadge status={turn.state} />
              </td>
              <Td mono align="right">
                {turn.state === "settled" ? (
                  <>
                    <Cr minor={turn.chargedMinor ?? 0} currency={display} /> ·{" "}
                    <span title={settlementOutcome(turn.kind).detail}>
                      {settlementOutcome(turn.kind).label}
                    </span>
                  </>
                ) : turn.state === "streaming" ? (
                  `${turn.outputChars ?? 0} chars out`
                ) : (
                  <>
                    frozen <Cr minor={turn.freezeMinor ?? 0} currency={display} />
                  </>
                )}
              </Td>
            </tr>
          ))}
        </TableShell>
      ) : null}

      {records.isError ? (
        <PageState>The requests could not be read.</PageState>
      ) : (
        <>
          <TableShell
            head={[
              "Booked",
              { label: "Request", truncate: true },
              "Model",
              "Key",
              "Status",
              { label: "In", align: "right" },
              { label: "Out", align: "right" },
              { label: "Charged", align: "right" },
              "",
            ]}
            loading={records.isLoading ? 8 : 0}
          >
            {!records.isLoading &&
              rows.map((row) => (
              <tr key={row.requestId}>
                <td>{row.bookedOn}</td>
                <Td mono truncate>{row.requestId}</Td>
                <td>
                  <button
                    type="button"
                    className="text-primary hover:underline"
                    onClick={() =>
                      setParams((p) => {
                        const next = new URLSearchParams(p);
                        next.set("model", row.model);
                        return next;
                      })
                    }
                  >
                    {row.model}
                  </button>
                </td>
                <td className="mono">
                  {row.keyId ? (
                    <button
                      type="button"
                      className="text-primary hover:underline"
                      onClick={() =>
                        setParams((p) => {
                          const next = new URLSearchParams(p);
                          const prefix = prefixOf.get(row.keyId!) ?? row.keyId!;
                          next.set("key", prefix);
                          return next;
                        })
                      }
                    >
                      {prefixOf.get(row.keyId!) ?? row.keyId!.slice(0, 8)}
                    </button>
                  ) : (
                    "—"
                  )}
                </td>
                <td>
                  <StatusBadge status={row.status} />
                  <span
                    className="ml-1 text-xs text-muted"
                    title={settlementOutcome(row.kind).detail}
                  >
                    {settlementOutcome(row.kind).label}
                  </span>
                  {row.failureReason ? (
                    <p className="text-xs text-danger">{row.failureReason}</p>
                  ) : null}
                </td>
                <Td mono align="right">{row.inputTokens}</Td>
                <Td mono align="right">{row.outputTokens}</Td>
                <Td mono align="right"><Cr minor={row.chargedMinor} currency={display} /></Td>
                <td>
                  <a href={`/verify?entry=${row.entryId}`} className="text-primary">
                    verify
                  </a>
                </td>
              </tr>
              ))}
            {!records.isLoading && rows.length === 0 ? (
              <EmptyRows colSpan={9} icon={<ArrowLeftRight />}>
                {keyFilter || modelFilter ? "No requests match the filters." : "No requests yet."}
              </EmptyRows>
            ) : null}
          </TableShell>
          {records.data?.nextCursor != null ? (
            <p>
              <Button tone="ghost" onClick={() => setBefore(records.data?.nextCursor)}>
                Older
              </Button>
            </p>
          ) : null}
        </>
      )}
    </div>
  );
}
