import { Link } from "react-router-dom";
import { useState } from "react";
import {
  useBalance,
  useBudget,
  useCreateHold,
  useEntries,
  useHolds,
  useSession,
  useSettleHold,
} from "../api/hooks";
import { HaloProgress } from "../components/cult/halo-progress";
import { HaloButton } from "../components/cult/halo-button";
import { HaloInput } from "../components/cult/halo-input";
import {
  AnimatedKpi,
  Banner,
  Card,
  EmptyRows,
  Field,
  Kpi,
  PageState,
  TableShell,
  credits,
} from "../components/ui";

export function Overview() {
  const session = useSession();
  const balance = useBalance();
  const budget = useBudget();
  const holds = useHolds();
  const entries = useEntries(10);

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Overview</h1>
        <Link
          to="/topup"
          className="rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-white hover:bg-primary-hover"
        >
          Top up
        </Link>
      </div>
      {!session.data?.user.emailVerified ? (
        <p className="rounded-md border border-pending/40 bg-amber-50 px-3 py-2 text-sm text-pending">
          Your email is not verified. Check your inbox for the verification mail.
        </p>
      ) : null}
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-4">
        {balance.data ? (
          <>
            <AnimatedKpi label="Available" minor={balance.data.availableMinor} />
            <AnimatedKpi
              label="Frozen"
              minor={balance.data.reservedMinor}
              hint="what in-flight holds reserve"
            />
            <AnimatedKpi label="Spent this month" minor={balance.data.spentThisMonthMinor} />
          </>
        ) : (
          <>
            <Kpi label="Available" value="…" />
            <Kpi label="Frozen" value="…" />
            <Kpi label="Spent this month" value="…" />
          </>
        )}
        <div className="rounded-md border border-line bg-surface p-4">
          <p className="text-xs text-muted">Credit line</p>
          <p className="mt-1 text-2xl font-semibold tabular-nums">
            {balance.data
              ? `${credits(balance.data.creditUsedMinor)} / ${credits(balance.data.creditLimitMinor)}`
              : "…"}{" "}
            <span className="text-sm font-normal text-muted">cr</span>
          </p>
          {budget.data?.organization.consumedPercent != null ? (
            <div className="mt-2 flex flex-col gap-1">
              <HaloProgress
                value={Math.min(100, budget.data.organization.consumedPercent)}
                className="w-full"
              />
              <p className="text-xs text-muted">
                {budget.data.organization.consumedPercent}% committed
              </p>
            </div>
          ) : (
            <p className="mt-1 text-xs text-muted">no credit line</p>
          )}
        </div>
      </div>

      {budget.data?.member ? (
        <Card title="Your member budget">
          <div className="flex flex-col gap-2">
            <p className="text-sm">
              {budget.data.member.consumedPercent ?? 0}% of{" "}
              {credits(budget.data.member.budgetLimitMinor)} cr committed this window.
            </p>
            <HaloProgress
              value={Math.min(100, budget.data.member.consumedPercent ?? 0)}
              className="w-full"
            />
          </div>
        </Card>
      ) : null}

      {budget.data && budget.data.keys.some((k) => k.spendLimitMinor != null) ? (
        <Card title="Key budgets">
          <TableShell head={["Key", "Window", "Committed", "Consumed"]}>
            {budget.data.keys
              .filter((k) => k.spendLimitMinor != null)
              .map((k) => (
                <tr key={k.keyId}>
                  <td>
                    {k.name ?? <code className="mono">{k.prefix}</code>}
                  </td>
                  <td>{k.budgetDuration ?? "cumulative"}</td>
                  <td className="mono">{credits(k.committedMinor)} cr</td>
                  <td>{k.consumedPercent != null ? `${k.consumedPercent}%` : "—"}</td>
                </tr>
              ))}
          </TableShell>
        </Card>
      ) : null}

      <Card
        title="In-flight holds"
        actions={
          <Link to="/requests" className="text-sm text-primary">
            Requests →
          </Link>
        }
      >
        {holds.isLoading ? (
          <PageState>Loading…</PageState>
        ) : (
          <TableShell head={["Request", "Model", "Key", "Frozen"]}>
            {(holds.data?.holds ?? []).map((hold) => (
              <tr key={hold.requestId}>
                <td className="mono">{hold.requestId}</td>
                <td>{hold.model}</td>
                <td className="mono">{hold.keyPrefix ?? "—"}</td>
                <td className="mono">{credits(hold.freezeMinor)} cr</td>
              </tr>
            ))}
            {(holds.data?.holds.length ?? 0) === 0 ? (
              <EmptyRows colSpan={4}>Nothing in flight.</EmptyRows>
            ) : null}
          </TableShell>
        )}
        <HoldBench />
      </Card>

      <Card
        title="Recent entries"
        actions={
          <Link to="/log" className="text-sm text-primary">
            Full log →
          </Link>
        }
      >
        {entries.isLoading ? (
          <PageState>Loading…</PageState>
        ) : (
          <TableShell head={["#", "Description"]}>
            {(entries.data?.rows ?? []).map((entry) => (
              <tr key={entry.id}>
                <td className="mono">{entry.index}</td>
                <td className="mono">{entry.description}</td>
              </tr>
            ))}
            {(entries.data?.rows.length ?? 0) === 0 ? (
              <EmptyRows colSpan={2}>No entries yet.</EmptyRows>
            ) : null}
          </TableShell>
        )}
      </Card>
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
    <div className="mt-3 flex flex-col gap-2 border-t border-line pt-3">
      <p className="text-xs text-muted">
        Manual hold (<code className="mono">POST /holds</code>,{" "}
        <code className="mono">POST /settlements</code>) — useful to see the
        freeze/settle pair land in the log.
      </p>
      <div className="flex flex-wrap items-end gap-2">
        <Field label="Hold (credits)">
          <HaloInput
            type="number"
            min="0"
            step="any"
            value={amount}
            onChange={(e) => setAmount(e.target.value)}
            className="w-28"
          />
        </Field>
        <HaloButton
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
        </HaloButton>
        <Field label="Actual (credits)">
          <HaloInput
            type="number"
            min="0"
            step="any"
            value={actual}
            onChange={(e) => setActual(e.target.value)}
            className="w-28"
          />
        </Field>
        <HaloButton
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
        </HaloButton>
        <HaloButton
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
        </HaloButton>
      </div>
      {msg ? <Banner tone={msg.includes("failed") ? "error" : "info"}>{msg}</Banner> : null}
    </div>
  );
}
