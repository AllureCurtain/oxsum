import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ChartColumn, Wrench } from "lucide-react";
import {
  useBusiness,
  useCapability,
  useCostCoverage,
  useDisplay,
  useFinancialFacts,
  useReconciliationExceptions,
  useReconciliationRuns,
  useRepairException,
  useRequestRun,
  useResolveException,
  useRunExceptions,
  useWaiveException,
  type ReconciliationException,
  type ReconciliationRun,
} from "../../api/hooks";
import {
  Banner,
  Button,
  Card,
  Cr,
  EmptyRows,
  Field,
  Kpi,
  NativeSelect,
  Skeleton,
  TableShell,
  Td,
  TextInput,
} from "../../components/ui";
import { AuditAction, DetailGrid, Field2, stateBadge } from "./biz";

// ---------------------------------------------------------------------------
// /app/business#business/reports + #business/exceptions — the operational
// resolution surfaces (T22): the financial-facts dashboard,
// cost-coverage breakdown, reconciliation runs and the exception work list
// with resolve/waive/repair — every repair names its audited action.
// ---------------------------------------------------------------------------

const FACT_KEYS = [
  "usageMinor",
  "ratedMinor",
  "entitlementMinor",
  "fundingMinor",
  "billedMinor",
  "earnedMinor",
  "deferredMinor",
  "discountMinor",
  "collectedMinor",
  "refundMinor",
  "receivableMinor",
] as const;

function FactsDashboard() {
  const { t } = useTranslation();
  const display = useDisplay();
  const [orgId, setOrgId] = useState("");
  const [model, setModel] = useState("");
  const [channel, setChannel] = useState("");
  const [basis, setBasis] = useState("");
  const facts = useFinancialFacts({
    organizationId: orgId || undefined,
    model: model || undefined,
    channel: channel || undefined,
    timeBasis: basis || undefined,
  });
  const d = facts.data;

  return (
    <Card title={t("biz.reports.factsTitle")}>
      <div className="mb-4 grid grid-cols-2 gap-2 sm:grid-cols-4">
        <Field label={t("biz.payments.fOrg")}>
          <TextInput value={orgId} onChange={(e) => setOrgId(e.target.value)} placeholder="org uuid" />
        </Field>
        <Field label={t("common.model")}>
          <TextInput value={model} onChange={(e) => setModel(e.target.value)} />
        </Field>
        <Field label={t("common.channel")}>
          <TextInput value={channel} onChange={(e) => setChannel(e.target.value)} />
        </Field>
        <Field label={t("biz.reports.timeBasis")}>
          <NativeSelect value={basis} onChange={(e) => setBasis(e.target.value)}>
            <option value="">{t("biz.reports.basisBooking")}</option>
            <option value="service">{t("biz.reports.basisService")}</option>
            <option value="payment">{t("biz.reports.basisPayment")}</option>
          </NativeSelect>
        </Field>
      </div>
      {facts.isLoading ? (
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          {FACT_KEYS.map((k) => <Skeleton key={k} className="h-20" />)}
        </div>
      ) : facts.isError || !d ? (
        <Banner tone="error">{t("biz.loadError")}</Banner>
      ) : (
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          {FACT_KEYS.map((k) => (
            <Kpi
              key={k}
              label={t(`biz.reports.fact.${k}`)}
              value={<Cr minor={d[k]} currency={display} />}
            />
          ))}
        </div>
      )}
    </Card>
  );
}

function Coverage() {
  const { t } = useTranslation();
  const display = useDisplay();
  const coverage = useCostCoverage({});
  const d = coverage.data;
  if (coverage.isLoading) return <Skeleton className="h-24 w-full" />;
  if (coverage.isError || !d) return <Banner tone="error">{t("biz.loadError")}</Banner>;
  const buckets: [string, typeof d.realAttempt][] = [
    ["realAttempt", d.realAttempt],
    ["cacheReuse", d.cacheReuse],
    ["estimate", d.estimate],
    ["missing", d.missing],
    ["providerReconciled", d.providerReconciled],
  ];
  return (
    <Card title={t("biz.reports.coverageTitle")}>
      <TableShell
        density="compact"
        head={[
          t("biz.reports.colBasis"),
          { label: t("common.turns"), align: "right" },
          { label: t("biz.reports.colCost"), align: "right" },
          { label: t("biz.reports.colUntracked"), align: "right" },
        ]}
        sticky={false}
      >
        {buckets.map(([key, b]) => (
          <tr key={key}>
            <td>{t(`biz.reports.basis.${key}`)}</td>
            <Td mono align="right">{b.turns}</Td>
            <Td mono align="right"><Cr minor={b.costMinor} currency={display} /></Td>
            <Td mono align="right">{b.untrackedTurns}</Td>
          </tr>
        ))}
      </TableShell>
    </Card>
  );
}

function RunRow({ run, onPick }: { run: ReconciliationRun; onPick: () => void }) {
  const { t } = useTranslation();
  return (
    <tr>
      <Td mono truncate>{run.runId.slice(0, 8)}</Td>
      <td>{run.kind}</td>
      <Td mono>{run.periodStart} → {run.periodEnd}</Td>
      <td>{stateBadge(run.state)}</td>
      <Td mono align="right">{run.exceptionCount}</Td>
      <Td mono>{run.startedAt.slice(0, 10)}</Td>
      <td className="text-right">
        <Button tone="ghost" onClick={onPick}>{t("biz.reports.view")}</Button>
      </td>
    </tr>
  );
}

function RunDetail({ run, onBack }: { run: ReconciliationRun; onBack: () => void }) {
  const { t } = useTranslation();
  const exceptions = useRunExceptions(run.runId);
  return (
    <div className="flex flex-col gap-4">
      <div>
        <button type="button" className="text-sm text-primary" onClick={onBack}>{t("biz.reports.back")}</button>
        <h3 className="mt-1 text-lg font-bold">{t("biz.reports.runTitle", { id: run.runId.slice(0, 8) })}</h3>
      </div>
      <Card title={t("biz.reports.runDiff")}>
        <DetailGrid>
          <Field2 label={t("biz.reports.period")}>{run.periodStart} → {run.periodEnd}</Field2>
          <Field2 label={t("common.status")}>{stateBadge(run.state)}</Field2>
          <Field2 label={t("biz.reports.exceptions")}>{run.exceptionCount}</Field2>
          <Field2 label={t("biz.reports.finished")}>{run.finishedAt?.slice(0, 10) ?? "—"}</Field2>
        </DetailGrid>
        {run.lastError ? <Banner tone="error" className="mt-3">{run.lastError}</Banner> : null}
      </Card>
      <Card title={t("biz.reports.runExceptions")}>
        {exceptions.isLoading ? (
          <Skeleton className="h-20 w-full" />
        ) : exceptions.isError ? (
          <Banner tone="error">{t("biz.loadError")}</Banner>
        ) : (
          <TableShell
            density="compact"
            head={[t("biz.exceptions.colKind"), t("biz.exceptions.colExpected"), t("biz.exceptions.colActual"), t("common.status"), t("biz.exceptions.colDetail")]}
            sticky={false}
          >
            {(exceptions.data ?? []).map((e) => (
              <tr key={e.exceptionId}>
                <td>{e.kind}</td>
                <Td mono align="right">{e.expectedMinor ?? "—"}</Td>
                <Td mono align="right">{e.actualMinor ?? "—"}</Td>
                <td>{stateBadge(e.state)}</td>
                <Td truncate>{e.detail ?? "—"}</Td>
              </tr>
            ))}
            {(exceptions.data?.length ?? 0) === 0 ? (
              <EmptyRows colSpan={5}>{t("biz.exceptions.empty")}</EmptyRows>
            ) : null}
          </TableShell>
        )}
      </Card>
    </div>
  );
}

function Runs() {
  const { t } = useTranslation();
  const runs = useReconciliationRuns();
  const request = useRequestRun();
  const canWrite = useCapability(useBusiness().data, "fundsWrite");
  const [picked, setPicked] = useState<ReconciliationRun | null>(null);
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");

  if (picked) {
    return <RunDetail run={picked} onBack={() => setPicked(null)} />;
  }
  return (
    <Card title={t("biz.reports.runsTitle")}>
      {canWrite ? (
        <form
          className="mb-4 grid grid-cols-2 gap-2 sm:grid-cols-4"
          onSubmit={(e) => {
            e.preventDefault();
            request.mutate({ idempotencyKey: crypto.randomUUID(), periodStart: from, periodEnd: to });
          }}
        >
          <Field label={t("biz.reports.colFrom")}>
            <TextInput type="date" value={from} onChange={(e) => setFrom(e.target.value)} required />
          </Field>
          <Field label={t("biz.reports.colTo")}>
            <TextInput type="date" value={to} onChange={(e) => setTo(e.target.value)} required />
          </Field>
          <div className="col-span-2 flex items-end">
            <Button type="submit" isLoading={request.isPending}>{t("biz.reports.requestRun")}</Button>
          </div>
        </form>
      ) : null}
      {runs.isError ? (
        <Banner tone="error">{t("biz.loadError")}</Banner>
      ) : (
        <TableShell
          head={[
            t("biz.reports.colId"),
            t("biz.reports.colKind"),
            t("biz.reports.colPeriod"),
            t("common.status"),
            { label: t("biz.reports.colExceptions"), align: "right" },
            t("biz.reports.colStarted"),
            "",
          ]}
          loading={runs.isLoading ? 4 : 0}
        >
          {!runs.isLoading &&
            (runs.data ?? []).map((r) => <RunRow key={r.runId} run={r} onPick={() => setPicked(r)} />)}
          {!runs.isLoading && (runs.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={7} icon={<ChartColumn />}>
              {t("biz.reports.noRuns")}
            </EmptyRows>
          ) : null}
        </TableShell>
      )}
    </Card>
  );
}

export function BusinessReports() {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-6">
      <h2 className="text-xl font-bold">{t("biz.reports.title")}</h2>
      <FactsDashboard />
      <Coverage />
      <Runs />
    </div>
  );
}

// ---------------------------------------------------------------------------
// exceptions — the open reconciliation-exception work list with the three
// audited closes (resolve / waive / repair). Each repair names its action.
// ---------------------------------------------------------------------------

const REPAIR_ACTIONS = ["reclassifyCost", "adjustJournal", "linkPayment", "markDuplicate"] as const;

function ExceptionRow({ e }: { e: ReconciliationException }) {
  const { t } = useTranslation();
  const resolve = useResolveException();
  const waive = useWaiveException();
  const repair = useRepairException();
  const canWrite = useCapability(useBusiness().data, "fundsWrite");
  const [action, setAction] = useState<(typeof REPAIR_ACTIONS)[number]>("reclassifyCost");

  return (
    <tr>
      <Td mono truncate>{e.exceptionId.slice(0, 8)}</Td>
      <td>{e.kind}</td>
      <Td mono truncate>{e.organizationId?.slice(0, 8) ?? "—"}</Td>
      <Td mono align="right">{e.expectedMinor ?? "—"}</Td>
      <Td mono align="right">{e.actualMinor ?? "—"}</Td>
      <td>{stateBadge(e.state)}</td>
      <Td truncate>{e.detail ?? "—"}</Td>
      <td className="text-right">
        {canWrite && e.state === "open" ? (
          <span className="inline-flex flex-wrap justify-end gap-1">
            <NativeSelect value={action} onChange={(ev) => setAction(ev.target.value as typeof action)} className="w-36">
              {REPAIR_ACTIONS.map((a) => <option key={a} value={a}>{a}</option>)}
            </NativeSelect>
            <AuditAction
              trigger={<Button tone="ghost">{t("biz.exceptions.repair")}</Button>}
              title={t("biz.exceptions.repairTitle")}
              description={t("biz.exceptions.repairExplain", { action })}
              confirmLabel={t("biz.exceptions.repair")}
              reasonLabel={t("biz.reason")}
              isPending={repair.isPending}
              error={repair.error}
              onConfirm={(reason, key) =>
                repair.mutate({ exceptionId: e.exceptionId, idempotencyKey: key, action, reason })
              }
            />
            <AuditAction
              trigger={<Button tone="ghost">{t("biz.exceptions.resolve")}</Button>}
              title={t("biz.exceptions.resolveTitle")}
              description={t("biz.exceptions.resolveExplain")}
              confirmLabel={t("biz.exceptions.resolve")}
              reasonLabel={t("biz.exceptions.resolution")}
              isPending={resolve.isPending}
              error={resolve.error}
              onConfirm={(resolution, key) =>
                resolve.mutate({ exceptionId: e.exceptionId, idempotencyKey: key, resolution })
              }
            />
            <AuditAction
              trigger={<Button tone="danger">{t("biz.exceptions.waive")}</Button>}
              title={t("biz.exceptions.waiveTitle")}
              description={t("biz.exceptions.waiveExplain")}
              confirmLabel={t("biz.exceptions.waive")}
              tone="danger"
              reasonLabel={t("biz.exceptions.resolution")}
              isPending={waive.isPending}
              error={waive.error}
              onConfirm={(resolution, key) =>
                waive.mutate({ exceptionId: e.exceptionId, idempotencyKey: key, resolution })
              }
            />
          </span>
        ) : null}
      </td>
    </tr>
  );
}

export function BusinessExceptions() {
  const { t } = useTranslation();
  const [openOnly, setOpenOnly] = useState(true);
  const exceptions = useReconciliationExceptions({ open: openOnly });

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <h2 className="text-xl font-bold">{t("biz.exceptions.title")}</h2>
        <label className="flex items-center gap-2 text-sm text-muted">
          <input type="checkbox" checked={openOnly} onChange={(e) => setOpenOnly(e.target.checked)} />
          {t("biz.exceptions.openOnly")}
        </label>
      </div>
      <Card title={t("biz.exceptions.listTitle")}>
        {exceptions.isError ? (
          <Banner tone="error">{t("biz.loadError")}</Banner>
        ) : (
          <TableShell
            head={[
              t("biz.exceptions.colId"),
              t("biz.exceptions.colKind"),
              t("biz.exceptions.colOrg"),
              { label: t("biz.exceptions.colExpected"), align: "right" },
              { label: t("biz.exceptions.colActual"), align: "right" },
              t("common.status"),
              t("biz.exceptions.colDetail"),
              "",
            ]}
            loading={exceptions.isLoading ? 5 : 0}
          >
            {!exceptions.isLoading &&
              (exceptions.data ?? []).map((e) => <ExceptionRow key={e.exceptionId} e={e} />)}
            {!exceptions.isLoading && (exceptions.data?.length ?? 0) === 0 ? (
              <EmptyRows colSpan={8} icon={<Wrench />}>
                {t("biz.exceptions.empty")}
              </EmptyRows>
            ) : null}
          </TableShell>
        )}
      </Card>
    </div>
  );
}
