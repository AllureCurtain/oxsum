import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
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
  const { t } = useTranslation();
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
        <h1 className="text-2xl font-bold">{t("requests.title")}</h1>
        {socket.dead ? (
          <button
            type="button"
            onClick={socket.reconnect}
            title={t("requests.reconnect")}
            className="text-xs font-medium text-danger hover:text-ink"
          >
            {t("requests.connectionLost")}
          </button>
        ) : (
          <span className={`text-xs ${socket.live ? "text-success" : "text-muted"}`}>
            {socket.live ? t("requests.live") : t("requests.offline")}
          </span>
        )}
      </div>

      <div className="flex flex-wrap gap-x-10 gap-y-4 rounded-xl border border-line bg-surface px-5 py-4 shadow-[0_1px_3px_rgba(20,34,64,.05)]">
        <StatChip
          label={t("requests.inFlight")}
          value={stats.inFlight}
          hint={stats.inFlight > 0 ? t("requests.turnsLive") : t("requests.none")}
        />
        <StatChip label={t("requests.turnsToday")} value={stats.todayTurns} />
        <StatChip
          label={t("requests.tokensToday")}
          value={stats.todayTokens.toLocaleString("en-US")}
          hint={t("requests.inOut")}
        />
        <StatChip
          label={t("requests.chargedToday")}
          value={<Cr minor={stats.todayCharged} currency={display} />}
        />
        <StatChip
          label={t("requests.avgTokens")}
          value={
            stats.todayTurns > 0 ? Math.round(stats.todayTokens / stats.todayTurns).toLocaleString("en-US") : "—"
          }
        />
      </div>

      <div className="flex flex-wrap gap-3">
        <TextInput
          placeholder={t("requests.keyPrefix")}
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
          placeholder={t("requests.model")}
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
            {t("requests.clearFilters")}
          </Button>
        ) : null}
      </div>

      {socket.turns.length > 0 ? (
        <TableShell
          head={[{ label: t("requests.colRequest"), truncate: true }, t("common.model"), t("requests.state"), { label: t("requests.colLive"), align: "right" }]}
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
                    <span title={settlementOutcome(turn.kind, t).detail}>
                      {settlementOutcome(turn.kind, t).label}
                    </span>
                  </>
                ) : turn.state === "streaming" ? (
                  t("requests.charsOut", { count: turn.outputChars ?? 0 })
                ) : (
                  <>
                    {t("requests.frozen")}{" "}<Cr minor={turn.freezeMinor ?? 0} currency={display} />
                  </>
                )}
              </Td>
            </tr>
          ))}
        </TableShell>
      ) : null}

      {records.isError ? (
        <PageState>{t("requests.readError")}</PageState>
      ) : (
        <>
          <TableShell
            head={[
              t("requests.colBooked"),
              { label: t("requests.colRequest"), truncate: true },
              t("common.model"),
              t("requests.colKey"),
              t("common.status"),
              { label: t("usage.in"), align: "right" },
              { label: t("usage.out"), align: "right" },
              { label: t("usage.charged"), align: "right" },
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
                    title={settlementOutcome(row.kind, t).detail}
                  >
                    {settlementOutcome(row.kind, t).label}
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
                    {t("common.verify")}
                  </a>
                </td>
              </tr>
              ))}
            {!records.isLoading && rows.length === 0 ? (
              <EmptyRows colSpan={9} icon={<ArrowLeftRight />}>
                {keyFilter || modelFilter ? t("requests.emptyFiltered") : t("requests.empty")}
              </EmptyRows>
            ) : null}
          </TableShell>
          {records.data?.nextCursor != null ? (
            <p>
              <Button tone="ghost" onClick={() => setBefore(records.data?.nextCursor)}>
                {t("common.older")}
              </Button>
            </p>
          ) : null}
        </>
      )}
    </div>
  );
}
