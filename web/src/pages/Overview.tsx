import { Link } from "react-router-dom";
import { useState } from "react";
import {
  useBalance,
  useBudget,
  useCreateHold,
  useDisplay,
  useEntries,
  useHolds,
  useSession,
  useSettleHold,
} from "../api/hooks";
import { Progress } from "../components/ui";
import { Button } from "../components/ui";
import { TextInput } from "../components/ui";
import {
  AnimatedKpi,
  Cr,
  Banner,
  Card,
  EmptyRows,
  Field,
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

      {/* KPI row */}
      <div className="grid grid-cols-1 gap-3.5 sm:grid-cols-2 lg:grid-cols-4">
        {balance.data ? (
          <>
            <AnimatedKpi label="Available" minor={balance.data.availableMinor} currency={display} />
            <AnimatedKpi
              label="Frozen"
              minor={balance.data.reservedMinor}
              hint="in-flight holds reserve"
              currency={display}
            />
            <AnimatedKpi
              label="Spent this month"
              minor={balance.data.spentThisMonthMinor}
              currency={display}
            />
          </>
        ) : (
          <>
            <AnimatedKpi label="Available" minor={0} />
            <AnimatedKpi label="Frozen" minor={0} />
            <AnimatedKpi label="Spent this month" minor={0} />
          </>
        )}
        <div className="rounded-xl border border-line bg-surface p-4 shadow-[0_1px_3px_rgba(20,34,64,.05)]">
          <p className="text-xs font-medium text-muted">Credit line</p>
          <p className="mt-1.5 text-2xl font-bold tabular-nums tracking-tight text-ink">
            {balance.data
              ? `${credits(balance.data.creditUsedMinor)} / ${credits(balance.data.creditLimitMinor)}`
              : "…"}{" "}
            <span className="text-sm font-normal text-muted">cr</span>
          </p>
          {balance.data && display ? (
            <p className="mt-0.5 text-xs text-muted">
              ≈ {displayAmount(balance.data.creditUsedMinor, display)} {display.code} used
            </p>
          ) : null}
          {budget.data?.organization.consumedPercent != null ? (
            <div className="mt-3 flex flex-col gap-1.5">
              <Progress
                value={Math.min(100, budget.data.organization.consumedPercent)}
                className="w-full"
              />
              <p className="text-[11px] text-muted">
                {budget.data.organization.consumedPercent}% committed
              </p>
            </div>
          ) : (
            <p className="mt-2 text-xs text-muted">no credit line</p>
          )}
        </div>
      </div>

      {budget.data?.member ? (
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
                  <Td align="right">{k.consumedPercent != null ? `${k.consumedPercent}%` : "—"}</Td>
                </tr>
              ))}
          </TableShell>
        </Card>
      ) : null}

      <div className="grid grid-cols-1 gap-3.5 xl:grid-cols-2">
        {/* In-flight holds */}
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
              <EmptyRows colSpan={4}>Nothing in flight.</EmptyRows>
            ) : null}
          </TableShell>
          <HoldBench />
        </Card>

        {/* Recent entries */}
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
              <EmptyRows colSpan={3}>No entries yet.</EmptyRows>
            ) : null}
          </TableShell>
        </Card>
      </div>
    </div>
  );
}

/** A hold you take by hand — the key is the idempotencyKey you pick, so the
 *  settle form below already knows what to name. Settling at 0 is the release. */
function HoldBench() {
  const [amount, setAmount] = useState("");
  const [holdKey, setHoldKey] = useState("");
  const [actual, setActual] = useState("0");
  const [msg, setMsg] = useState<string | null>(null);
  const hold = useCreateHold();
  const settle = useSettleHold();

  return (
    <div className="mt-3 flex flex-col gap-2.5 border-t border-line pt-3.5">
      <p className="text-xs text-muted">
        Manual hold (<code className="mono">POST /holds</code>,{" "}
        <code className="mono">POST /settlements</code>) — useful to see the
        freeze/settle pair land in the log.
      </p>
      <div className="flex flex-wrap items-end gap-2">
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
    </div>
  );
}
