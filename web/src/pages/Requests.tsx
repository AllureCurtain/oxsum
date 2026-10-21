import { useMemo, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { settlementOutcome } from "../lib/kinds";
import { useBillingRecords, useDisplay, useKeys } from "../api/hooks";
import { useBillingSocket } from "../api/ws";
import {
  EmptyRows,
  PageState,
  StatusBadge,
  TableShell,
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

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Requests</h1>
        <span className={`text-xs ${socket.live ? "text-success" : "text-muted"}`}>
          {socket.live ? "live" : "offline"}
        </span>
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
        <TableShell head={["Request", "Model", "State", "Live"]}>
          {socket.turns.map((turn) => (
            <tr key={turn.requestId}>
              <td className="mono">{turn.requestId}</td>
              <td>{turn.model}</td>
              <td>
                <StatusBadge status={turn.state} />
              </td>
              <td className="mono">
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
              </td>
            </tr>
          ))}
        </TableShell>
      ) : null}

      {records.isLoading ? (
        <PageState>Loading…</PageState>
      ) : records.isError ? (
        <PageState>The requests could not be read.</PageState>
      ) : (
        <>
          <TableShell head={["Booked", "Request", "Model", "Key", "Status", "In", "Out", "Charged", ""]}>
            {rows.map((row) => (
              <tr key={row.requestId}>
                <td>{row.bookedOn}</td>
                <td className="mono">{row.requestId}</td>
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
                      {prefixOf.get(row.keyId) ?? row.keyId.slice(0, 8)}
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
                <td className="mono">{row.inputTokens}</td>
                <td className="mono">{row.outputTokens}</td>
                <td className="mono"><Cr minor={row.chargedMinor} currency={display} /></td>
                <td>
                  <a href={`/verify?entry=${row.entryId}`} className="text-primary">
                    verify
                  </a>
                </td>
              </tr>
            ))}
            {rows.length === 0 ? (
              <EmptyRows colSpan={9}>
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
