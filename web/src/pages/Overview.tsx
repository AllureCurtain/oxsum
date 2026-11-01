import { Link } from "react-router-dom";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ArrowDownRight,
  ArrowUpRight,
  CreditCard,
  FlaskConical,
  Snowflake,
  TrendingUp,
  Wallet,
} from "lucide-react";
import {
  useBalance,
  useBudget,
  useCreateHold,
  useDisplay,
  useEntries,
  useHolds,
  useSession,
  useSettleHold,
  useUsage,
} from "../api/hooks";
import { Progress } from "../components/ui";
import { Button } from "../components/ui";
import { TextInput } from "../components/ui";
import {
  Cr,
  Banner,
  Card,
  EmptyRows,
  Field,
  Skeleton,
  Sparkline,
  StatChip,
  TableShell,
  Td,
  credits,
  displayAmount,
} from "../components/ui";

export function Overview() {
  const { t } = useTranslation();
  const session = useSession();
  const balance = useBalance();
  const budget = useBudget();
  const holds = useHolds();
  const entries = useEntries(10);
  const display = useDisplay();
  const today = new Date().toISOString().slice(0, 10);
  const weekAgo = new Date(Date.now() - 6 * 86_400_000).toISOString().slice(0, 10);
  const usage = useUsage(weekAgo, today);

  // The API's `day` is the wire's `[year, dayOfYear]` pair; normalize it to the
  // ISO date the rest of the page speaks.
  const dayOf = (row: { day: string | [number, number] }) => {
    if (typeof row.day === "string") return row.day;
    const [y, doy] = row.day;
    const d = new Date(Date.UTC(y, 0, doy));
    return d.toISOString().slice(0, 10);
  };

  // The trailing-7-day charged series the sparkline reads — one entry per UTC
  // day, zero-filled so a quiet week still draws a flat line.
  const spark = useMemo(() => {
    const byDay = new Map<string, number>();
    for (const row of usage.data?.days ?? []) {
      const d = dayOf(row);
      byDay.set(d, (byDay.get(d) ?? 0) + row.chargedMinor);
    }
    const series: number[] = [];
    for (let i = 6; i >= 0; i--) {
      const d = new Date(Date.now() - i * 86_400_000).toISOString().slice(0, 10);
      series.push(byDay.get(d) ?? 0);
    }
    return series;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [usage.data]);

  // Today's shape: turns and tokens summed across every (channel, model, key)
  // bucket that booked under today's date. Yesterday is the comparison a
  // single day never carries on its own.
  const todayStats = useMemo(() => {
    let turns = 0;
    let charged = 0;
    let tokens = 0;
    for (const row of usage.data?.days ?? []) {
      if (dayOf(row) === today) {
        turns += row.turns;
        charged += row.chargedMinor;
        tokens += row.inputTokens + row.outputTokens;
      }
    }
    let yesterdayCharged = 0;
    const yesterday = new Date(Date.now() - 86_400_000).toISOString().slice(0, 10);
    for (const row of usage.data?.days ?? []) {
      if (dayOf(row) === yesterday) yesterdayCharged += row.chargedMinor;
    }
    return { turns, charged, tokens, yesterdayCharged };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [usage.data, today]);

  return (
    <div className="flex flex-col gap-5">
      <div className="flex items-center justify-between">
        <h1 className="text-xl font-bold tracking-tight text-ink">{t("overview.title")}</h1>
        <Link
          to="/topup"
          className="rounded-lg bg-primary px-3.5 py-2 text-sm font-medium text-white shadow-[0_2px_8px_rgba(45,91,216,.3)] transition-colors hover:bg-primary-hover"
        >
          {t("topup.title")}
        </Link>
      </div>

      {!session.data?.user.emailVerified ? (
        <Banner tone="warn">{t("overview.emailUnverified")}</Banner>
      ) : null}

      {/* Hero: the balance carries the visual weight — the number that answers
          "can I spend" plus the week's shape; the right rail is the operational
          context (in-flight reserve, month-to-date spend, credit line). */}
      <div className="grid grid-cols-1 gap-3.5 lg:grid-cols-3">
        <Card className="lg:col-span-2">
          <div className="flex flex-col gap-5">
            <div className="flex items-start justify-between gap-4">
              <div>
                <p className="text-xs font-medium uppercase tracking-wider text-muted">
                  {t("overview.availableBalance")}
                </p>
                {balance.data ? (
                  <>
                    <p className="mt-2 text-4xl font-bold tabular-nums tracking-tight text-ink">
                      {credits(balance.data.availableMinor)}
                      <span className="ml-1.5 text-xl font-medium text-muted">cr</span>
                    </p>
                    {display ? (
                      <p className="mt-1 text-sm text-muted">
                        ≈ {displayAmount(balance.data.availableMinor, display)} {display.code}
                      </p>
                    ) : null}
                  </>
                ) : (
                  <>
                    <Skeleton className="mt-2 h-10 w-48" />
                    <Skeleton className="mt-2 h-4 w-32" />
                  </>
                )}
              </div>
              <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-primary-soft text-primary">
                <Wallet className="size-5" />
              </span>
            </div>
            <div className="border-t border-line pt-4">
              <div className="flex items-end gap-8">
                <div className="shrink-0 pb-0.5">
                  <p className="text-[11px] font-medium uppercase tracking-wider text-muted">
                    {t("overview.last7Days")}
                  </p>
                  <div className="mt-2 flex gap-7">
                    <StatChip
                      label={t("overview.today")}
                      value={<Cr minor={todayStats.charged} currency={display} />}
                      hint={
                        todayStats.yesterdayCharged > 0 ? (
                          <span className="inline-flex items-center gap-0.5">
                            {todayStats.charged >= todayStats.yesterdayCharged ? (
                              <ArrowUpRight className="size-3 text-danger" />
                            ) : (
                              <ArrowDownRight className="size-3 text-success" />
                            )}
                            {Math.round(
                              Math.abs(
                                ((todayStats.charged - todayStats.yesterdayCharged) /
                                  todayStats.yesterdayCharged) * 100,
                              ),
                            )}
                            %
                          </span>
                        ) : null
                      }
                    />
                    <StatChip label={t("overview.turns")} value={todayStats.turns} />
                    <StatChip
                      label={t("overview.tokens")}
                      value={todayStats.tokens.toLocaleString("en-US")}
                    />
                  </div>
                </div>
                <Sparkline
                  values={spark}
                  width={560}
                  height={72}
                  className="h-16 min-w-0 flex-1 self-end"
                />
              </div>
            </div>
          </div>
        </Card>

        {/* The operational rail: what is already committed, what has been spent
            this month, and the credit line's draw — each with its icon tile so
            the rail reads as a column of facts, not a wall of numbers. */}
        <div className="flex flex-col gap-3.5">
          <Card className="flex-1">
            <div className="flex h-full items-start justify-between gap-3">
              <div className="min-w-0">
                <p className="text-xs font-medium text-muted">{t("overview.frozenByHolds")}</p>
                {balance.data ? (
                  <p className="mt-1.5 text-2xl font-bold tabular-nums tracking-tight text-ink">
                    <Cr minor={balance.data.reservedMinor} currency={display} />
                  </p>
                ) : (
                  <Skeleton className="mt-1.5 h-8 w-24" />
                )}
                <p className="mt-1 text-xs text-muted">
                  {t("overview.holdsInFlight", { count: holds.data?.holds.length ?? 0 })}
                </p>
              </div>
              <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary-soft text-primary">
                <Snowflake className="size-4" />
              </span>
            </div>
          </Card>
          <Card className="flex-1">
            <div className="flex h-full items-start justify-between gap-3">
              <div className="min-w-0">
                <p className="text-xs font-medium text-muted">{t("overview.spentThisMonth")}</p>
                {balance.data ? (
                  <p className="mt-1.5 text-2xl font-bold tabular-nums tracking-tight text-ink">
                    <Cr minor={balance.data.spentThisMonthMinor} currency={display} />
                  </p>
                ) : (
                  <Skeleton className="mt-1.5 h-8 w-24" />
                )}
                <p className="mt-1 text-xs text-muted">{t("overview.settledCharges")}</p>
              </div>
              <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary-soft text-primary">
                <TrendingUp className="size-4" />
              </span>
            </div>
          </Card>
          <Card className="flex-1">
            <div className="flex h-full items-start justify-between gap-3">
              <div className="min-w-0 flex-1">
                <p className="text-xs font-medium text-muted">{t("overview.creditLine")}</p>
                {balance.data ? (
                  balance.data.creditLimitMinor > 0 ? (
                    <>
                      <p className="mt-1.5 text-2xl font-bold tabular-nums tracking-tight text-ink">
                        {credits(balance.data.creditUsedMinor)}{" "}
                        <span className="text-base font-normal text-muted">
                          / {credits(balance.data.creditLimitMinor)} cr
                        </span>
                      </p>
                      {budget.data?.organization.consumedPercent != null ? (
                        <div className="mt-2">
                          <Progress
                            value={Math.min(100, budget.data.organization.consumedPercent)}
                            className="w-full"
                          />
                          <p className="mt-1 text-[11px] text-muted">
                            {t("overview.percentCommitted", {
                              percent: budget.data.organization.consumedPercent,
                            })}
                          </p>
                        </div>
                      ) : null}
                    </>
                  ) : (
                    <p className="mt-1.5 text-sm leading-6 text-muted">
                      {t("overview.noCreditLine")}
                    </p>
                  )
                ) : (
                  <Skeleton className="mt-1.5 h-8 w-32" />
                )}
              </div>
              <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary-soft text-primary">
                <CreditCard className="size-4" />
              </span>
            </div>
          </Card>
        </div>
      </div>

      {/* The grid of working surfaces: in-flight turns beside the ledger's
          latest entries — the live half and the recorded half, side by side. */}
      <div className="grid grid-cols-1 gap-3.5 xl:grid-cols-2">
        <Card
          title={t("overview.inFlightHolds")}
          actions={
            <Link to="/requests" className="text-[13px] font-medium text-primary hover:underline">
              {t("overview.requestsLink")}
            </Link>
          }
        >
          <TableShell
            head={[
              { label: t("overview.colRequest"), truncate: true },
              t("common.model"),
              t("overview.colKey"),
              { label: t("overview.colFrozen"), align: "right" },
            ]}
            loading={holds.isLoading ? 4 : 0}
            sticky={false}
          >
            {!holds.isLoading &&
              (holds.data?.holds ?? []).map((hold) => (
                <tr key={hold.requestId}>
                  <Td mono truncate>{hold.requestId}</Td>
                  <td>{hold.model}</td>
                  <td className="mono">{hold.keyPrefix ?? "—"}</td>
                  <Td mono align="right">
                    <Cr minor={hold.freezeMinor} currency={display} />
                  </Td>
                </tr>
              ))}
            {!holds.isLoading && (holds.data?.holds.length ?? 0) === 0 ? (
              <EmptyRows colSpan={4} icon={<Snowflake />}>
                {t("overview.emptyHolds")}
              </EmptyRows>
            ) : null}
          </TableShell>
          <HoldBench />
        </Card>

        <Card
          title={t("overview.recentEntries")}
          actions={
            <Link to="/log" className="text-[13px] font-medium text-primary hover:underline">
              {t("overview.fullLogLink")}
            </Link>
          }
        >
          <TableShell
            head={[
              "#",
              { label: t("overview.colDescription"), truncate: true },
              { label: t("overview.colHash"), truncate: true },
            ]}
            loading={entries.isLoading ? 4 : 0}
            sticky={false}
          >
            {!entries.isLoading &&
              (entries.data?.rows ?? []).map((entry) => (
                <tr key={entry.id}>
                  <td className="mono">{entry.index}</td>
                  <Td mono truncate className="text-muted">
                    {entry.description}
                  </Td>
                  <Td mono truncate className="text-muted">
                    {entry.contentHash ?? ""}
                  </Td>
                </tr>
              ))}
            {!entries.isLoading && (entries.data?.rows.length ?? 0) === 0 ? (
              <EmptyRows colSpan={3} icon={<TrendingUp />}>
                {t("overview.emptyEntries")}
              </EmptyRows>
            ) : null}
          </TableShell>
        </Card>
      </div>

      {/* Constraints — budgets that bound the spend the hero is counting. Only
          render when something is actually capped. */}
      {budget.data?.member?.budgetLimitMinor != null ||
      (budget.data && budget.data.keys.some((k) => k.spendLimitMinor != null)) ? (
        <div className="grid grid-cols-1 gap-3.5 lg:grid-cols-2">
          {budget.data?.member?.budgetLimitMinor != null ? (
            <Card title={t("overview.yourMemberBudget")}>
              <div className="flex flex-col gap-2.5">
                <p className="text-sm text-ink">
                  {t("overview.budgetCommitted", {
                    percent: budget.data.member.consumedPercent ?? 0,
                  })}{" "}
                  <strong className="tabular-nums">
                    <Cr minor={budget.data.member.budgetLimitMinor} currency={display} />
                  </strong>
                </p>
                <Progress
                  value={Math.min(100, budget.data.member.consumedPercent ?? 0)}
                  className="w-full"
                />
              </div>
            </Card>
          ) : null}
          {budget.data && budget.data.keys.some((k) => k.spendLimitMinor != null) ? (
            <Card title={t("overview.keyBudgets")}>
              <TableShell
                head={[
                  t("overview.colKey"),
                  t("overview.colWindow"),
                  { label: t("overview.colCommitted"), align: "right" },
                  { label: t("overview.colConsumed"), align: "right" },
                ]}
                sticky={false}
              >
                {budget.data.keys
                  .filter((k) => k.spendLimitMinor != null)
                  .map((k) => (
                    <tr key={k.keyId}>
                      <td>{k.name ?? <code className="mono">{k.prefix}</code>}</td>
                      <td>{k.budgetDuration ?? t("overview.cumulative")}</td>
                      <Td mono align="right">
                        <Cr minor={k.committedMinor} currency={display} />
                      </Td>
                      <Td align="right">
                        {k.consumedPercent != null ? `${k.consumedPercent}%` : "—"}
                      </Td>
                    </tr>
                  ))}
              </TableShell>
            </Card>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/** A hold you take by hand — the key is the idempotencyKey you pick, so the
 *  settle form below already knows what to name. Settling at 0 is the release.
 *  This is a dev bench (`POST /holds` / `POST /settlements`), not a product
 *  action — it collapses behind a `<details>` so the read surface stays read. */
function HoldBench() {
  const { t } = useTranslation();
  const [amount, setAmount] = useState("");
  const [holdKey, setHoldKey] = useState("");
  const [actual, setActual] = useState("0");
  const [msg, setMsg] = useState<{ text: string; failed: boolean } | null>(null);
  const hold = useCreateHold();
  const settle = useSettleHold();

  return (
    <details className="mt-3 border-t border-line pt-3.5">
      <summary className="flex cursor-pointer list-none items-center gap-2 text-xs font-medium text-muted hover:text-ink [&::-webkit-details-marker]:hidden">
        <FlaskConical className="size-3.5" />
        {t("overview.holdBench")}
      </summary>
      <div className="mt-2.5 flex flex-wrap items-end gap-2">
        <Field label={t("overview.holdCredits")}>
          <TextInput
            type="number"
            min="0"
            step="any"
            value={amount}
            onChange={(e) => setAmount(e.target.value)}
            className="w-28"
          />
        </Field>
        <Button
          type="button"
          isLoading={hold.isPending}
          loadingText={t("overview.holding")}
          onClick={async () => {
            setMsg(null);
            const key = `manual-${Date.now()}`;
            try {
              await hold.mutateAsync({
                idempotencyKey: key,
                amountMinor: Math.round(Number(amount) * 1_000_000),
              });
              setHoldKey(key);
              setMsg({ text: t("overview.benchHeld", { key }), failed: false });
            } catch (err) {
              setMsg({
                text: err instanceof Error ? err.message : t("overview.benchHoldFailed"),
                failed: true,
              });
            }
          }}
        >
          {t("overview.benchHold")}
        </Button>
        <Field label={t("overview.actualCredits")}>
          <TextInput
            type="number"
            min="0"
            step="any"
            value={actual}
            onChange={(e) => setActual(e.target.value)}
            className="w-28"
          />
        </Field>
        <Button
          type="button"
          disabled={!holdKey}
          isLoading={settle.isPending}
          loadingText={t("overview.settling")}
          onClick={async () => {
            setMsg(null);
            try {
              await settle.mutateAsync({
                holdKey,
                actualMinor: Math.round(Number(actual) * 1_000_000),
              });
              setMsg({ text: t("overview.benchSettled", { key: holdKey }), failed: false });
              setHoldKey("");
            } catch (err) {
              setMsg({
                text: err instanceof Error ? err.message : t("overview.benchSettleFailed"),
                failed: true,
              });
            }
          }}
        >
          {t("overview.benchSettle")}
        </Button>
        <Button
          type="button"
          disabled={!holdKey}
          onClick={async () => {
            setMsg(null);
            try {
              await settle.mutateAsync({ holdKey, actualMinor: 0 });
              setMsg({ text: t("overview.benchReleased", { key: holdKey }), failed: false });
              setHoldKey("");
            } catch (err) {
              setMsg({
                text: err instanceof Error ? err.message : t("overview.benchReleaseFailed"),
                failed: true,
              });
            }
          }}
        >
          {t("overview.benchRelease")}
        </Button>
      </div>
      {msg ? <Banner tone={msg.failed ? "error" : "info"}>{msg.text}</Banner> : null}
    </details>
  );
}
