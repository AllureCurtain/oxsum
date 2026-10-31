import { Link } from "react-router-dom";
import { useMemo, useState } from "react";
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
        <h1 className="text-xl font-bold tracking-tight text-ink">Overview</h1>
        <Link
          to="/topup"
          className="rounded-lg bg-primary px-3.5 py-2 text-sm font-medium text-white shadow-[0_2px_8px_rgba(45,91,216,.3)] transition-colors hover:bg-primary-hover"
        >
          Top up
        </Link>
      </div>

      {!session.data?.user.emailVerified ? (
        <Banner tone="warn">
          Your email is not verified. Check your inbox for the verification mail.
        </Banner>
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
                  Available balance
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
                    Last 7 days
                  </p>
                  <div className="mt-2 flex gap-7">
                    <StatChip
                      label="Today"
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
                    <StatChip label="Turns" value={todayStats.turns} />
                    <StatChip
                      label="Tokens"
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
                <p className="text-xs font-medium text-muted">Frozen by open holds</p>
                {balance.data ? (
                  <p className="mt-1.5 text-2xl font-bold tabular-nums tracking-tight text-ink">
                    <Cr minor={balance.data.reservedMinor} currency={display} />
                  </p>
                ) : (
                  <Skeleton className="mt-1.5 h-8 w-24" />
                )}
                <p className="mt-1 text-xs text-muted">
                  {holds.data?.holds.length ?? 0} hold
                  {(holds.data?.holds.length ?? 0) === 1 ? "" : "s"} in flight
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
                <p className="text-xs font-medium text-muted">Spent this month</p>
                {balance.data ? (
                  <p className="mt-1.5 text-2xl font-bold tabular-nums tracking-tight text-ink">
                    <Cr minor={balance.data.spentThisMonthMinor} currency={display} />
                  </p>
                ) : (
                  <Skeleton className="mt-1.5 h-8 w-24" />
                )}
                <p className="mt-1 text-xs text-muted">settled charges</p>
              </div>
              <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary-soft text-primary">
                <TrendingUp className="size-4" />
              </span>
            </div>
          </Card>
          <Card className="flex-1">
            <div className="flex h-full items-start justify-between gap-3">
              <div className="min-w-0 flex-1">
                <p className="text-xs font-medium text-muted">Credit line</p>
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
                            {budget.data.organization.consumedPercent}% committed
                          </p>
                        </div>
                      ) : null}
                    </>
                  ) : (
                    <p className="mt-1.5 text-sm leading-6 text-muted">
                      No credit line — the wallet spends prepaid balance only.
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
          title="In-flight holds"
          actions={
            <Link to="/requests" className="text-[13px] font-medium text-primary hover:underline">
              Requests →
            </Link>
          }
        >
          <TableShell
            head={[
              { label: "Request", truncate: true },
              "Model",
              "Key",
              { label: "Frozen", align: "right" },
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
                Nothing in flight — holds appear here while a request is reserving credits.
              </EmptyRows>
            ) : null}
          </TableShell>
          <HoldBench />
        </Card>

        <Card
          title="Recent entries"
          actions={
            <Link to="/log" className="text-[13px] font-medium text-primary hover:underline">
              Full log →
            </Link>
          }
        >
          <TableShell
            head={["#", { label: "Description", truncate: true }, { label: "Hash", truncate: true }]}
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
                No entries yet — settlements land in the ledger as they book.
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
            <Card title="Your member budget">
              <div className="flex flex-col gap-2.5">
                <p className="text-sm text-ink">
                  {budget.data.member.consumedPercent ?? 0}% of{" "}
                  <strong className="tabular-nums">
                    <Cr minor={budget.data.member.budgetLimitMinor} currency={display} />
                  </strong>{" "}
                  committed this window.
                </p>
                <Progress
                  value={Math.min(100, budget.data.member.consumedPercent ?? 0)}
                  className="w-full"
                />
              </div>
            </Card>
          ) : null}
          {budget.data && budget.data.keys.some((k) => k.spendLimitMinor != null) ? (
            <Card title="Key budgets">
              <TableShell
                head={[
                  "Key",
                  "Window",
                  { label: "Committed", align: "right" },
                  { label: "Consumed", align: "right" },
                ]}
                sticky={false}
              >
                {budget.data.keys
                  .filter((k) => k.spendLimitMinor != null)
                  .map((k) => (
                    <tr key={k.keyId}>
                      <td>{k.name ?? <code className="mono">{k.prefix}</code>}</td>
                      <td>{k.budgetDuration ?? "cumulative"}</td>
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
  const [amount, setAmount] = useState("");
  const [holdKey, setHoldKey] = useState("");
  const [actual, setActual] = useState("0");
  const [msg, setMsg] = useState<string | null>(null);
  const hold = useCreateHold();
  const settle = useSettleHold();

  return (
    <details className="mt-3 border-t border-line pt-3.5">
      <summary className="flex cursor-pointer list-none items-center gap-2 text-xs font-medium text-muted hover:text-ink [&::-webkit-details-marker]:hidden">
        <FlaskConical className="size-3.5" />
        Manual hold bench — dev tool, freezes real credits
      </summary>
      <div className="mt-2.5 flex flex-wrap items-end gap-2">
        <Field label="Hold (credits)">
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
          loadingText="Holding…"
          onClick={async () => {
            setMsg(null);
            const key = `manual-${Date.now()}`;
            try {
              await hold.mutateAsync({
                idempotencyKey: key,
                amountMinor: Math.round(Number(amount) * 1_000_000),
              });
              setHoldKey(key);
              setMsg(`held — key ${key}`);
            } catch (err) {
              setMsg(err instanceof Error ? err.message : "hold failed");
            }
          }}
        >
          Hold
        </Button>
        <Field label="Actual (credits)">
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
          loadingText="Settling…"
          onClick={async () => {
            setMsg(null);
            try {
              await settle.mutateAsync({
                holdKey,
                actualMinor: Math.round(Number(actual) * 1_000_000),
              });
              setMsg(`settled ${holdKey}`);
              setHoldKey("");
            } catch (err) {
              setMsg(err instanceof Error ? err.message : "settle failed");
            }
          }}
        >
          Settle
        </Button>
        <Button
          type="button"
          disabled={!holdKey}
          onClick={async () => {
            setMsg(null);
            try {
              await settle.mutateAsync({ holdKey, actualMinor: 0 });
              setMsg(`released ${holdKey}`);
              setHoldKey("");
            } catch (err) {
              setMsg(err instanceof Error ? err.message : "release failed");
            }
          }}
        >
          Release
        </Button>
      </div>
      {msg ? <Banner tone={msg.includes("failed") ? "error" : "info"}>{msg}</Banner> : null}
    </details>
  );
}
