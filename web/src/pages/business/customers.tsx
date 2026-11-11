import { CustomerCreditCard } from "./customer-credit";
import { CustomerMonthlyStatements } from "./monthly-statements";
import { CustomerInvitations } from "./customer-invitations";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Users } from "lucide-react";
import { ApiError } from "../../api/client";
import {
  idem,
  useBindCustomerPlan,
  useBusinessCustomer,
  useBusinessCustomers,
  useBusinessOrders,
  useBusinessPlans,
  useBusinessStatements,
  useCancelCustomerPlan,
  useCapability,
  useBusiness,
  useCustomerLifecycle,
  useCustomerPlan,
  useCustomerSuspension,
  useDisplay,
  useUpdateCustomer,
  type BusinessCustomer,
} from "../../api/hooks";
import {
  Badge,
  Banner,
  Button,
  Card,
  Cr,
  EmptyRows,
  Field,
  NativeSelect,
  Skeleton,
  TableShell,
  Td,
  TextInput,
} from "../../components/ui";
import { AuditAction, DetailGrid, Field2, stateBadge } from "./biz";

// ---------------------------------------------------------------------------
// /app/business#business/customers — the customer roster + one customer's
// detail. Detail drills to its plan binding, statements and orders; a staff
// member with the right capability edits, suspends or archives.
// ---------------------------------------------------------------------------

function CustomerList({ onPick }: { onPick: (c: BusinessCustomer) => void }) {
  const { t } = useTranslation();
  const [after, setAfter] = useState<string | undefined>();
  const customers = useBusinessCustomers(after);
  const page = customers.data;

  return (
    <Card title={t("biz.customers.listTitle")}>
      {customers.isError ? (
        <Banner tone="error">{t("biz.loadError")}</Banner>
      ) : (
        <TableShell
          head={[
            t("biz.customers.colName"),
            t("biz.customers.colExternal"),
            t("biz.customers.colEmail"),
            t("common.status"),
            t("biz.customers.colSuspended"),
            { label: t("biz.customers.colSince"), align: "right" },
          ]}
          loading={customers.isLoading ? 5 : 0}
        >
          {!customers.isLoading &&
            (page?.customers ?? []).map((c) => (
              <tr key={c.organizationId}>
                <td>
                  <button type="button" className="font-medium text-primary" onClick={() => onPick(c)}>
                    {c.name}
                  </button>
                </td>
                <Td mono truncate>{c.externalId}</Td>
                <Td truncate>{c.contactEmail ?? "—"}</Td>
                <td>{stateBadge(c.state)}</td>
                <td>
                  {c.suspended ? <Badge tone="danger">{t("biz.customers.suspended")}</Badge> : "—"}
                </td>
                <Td mono align="right">{c.createdAt.slice(0, 10)}</Td>
              </tr>
            ))}
          {!customers.isLoading && (page?.customers.length ?? 0) === 0 ? (
            <EmptyRows colSpan={6} icon={<Users />}>
              {t("biz.customers.empty")}
            </EmptyRows>
          ) : null}
        </TableShell>
      )}
      {page?.nextCursor ? (
        <Button tone="ghost" className="mt-3" onClick={() => setAfter(page.nextCursor ?? undefined)}>
          {t("common.loadMore")}
        </Button>
      ) : null}
    </Card>
  );
}

/** The plan-binding card on a customer detail: the live `customerPlan`, its
 * credit standing, the bind form (priced → pendingPayment, free → grant) and
 * the boundary-cancel. */
function CustomerPlanCard({ customer }: { customer: BusinessCustomer }) {
  const { t } = useTranslation();
  const display = useDisplay();
  const plan = useCustomerPlan(customer.organizationId);
  const plans = useBusinessPlans(useCapability(useBusiness().data, "catalogWrite"));
  const bind = useBindCustomerPlan(customer.organizationId);
  const cancel = useCancelCustomerPlan(customer.organizationId);
  const [target, setTarget] = useState("");
  const [grant, setGrant] = useState("");

  const binding = plan.data?.plan;
  const credit = plan.data?.credit;
  const published = (plans.data ?? []).filter((p) => p.publishedAt != null && p.retiredAt == null);
  const chosen = published.find((p) => p.planVersionId === target);
  const canWrite = useCapability(useBusiness().data, "commercialWrite");

  return (
    <Card title={t("biz.customers.planTitle")}>
      {plan.isLoading ? (
        <Skeleton className="h-20 w-full" />
      ) : plan.isError ? (
        <Banner tone="error">{t("biz.loadError")}</Banner>
      ) : binding ? (
        <div className="flex flex-col gap-3">
          <div className="flex flex-wrap items-center gap-2">
            <p className="text-sm font-semibold text-ink">
              {(plans.data ?? []).find((p) => p.planVersionId === binding.planVersionId)?.name ??
                t("plans.unnamed")}
            </p>
            {stateBadge(binding.state)}
            {binding.cancelAtPeriodEnd ? <Badge tone="secondary">{t("plans.cancelAtPeriodEnd")}</Badge> : null}
          </div>
          <DetailGrid>
            <Field2 label={t("biz.customers.activated")}>
              {binding.activatedAt?.slice(0, 10) ?? "—"}
            </Field2>
            <Field2 label={t("plans.creditLimit")}>
              {credit?.creditLimitMinor != null ? (
                <Cr minor={credit.creditLimitMinor} currency={display} />
              ) : (
                t("plans.unlimited")
              )}
            </Field2>
            <Field2 label={t("plans.outstanding")}>
              {credit ? <Cr minor={credit.outstandingMinor} currency={display} /> : "—"}
            </Field2>
            <Field2 label={t("plans.nextDue")}>{credit?.nextDueAt?.slice(0, 10) ?? "—"}</Field2>
          </DetailGrid>
          {canWrite && binding.state !== "cancelled" && !binding.cancelAtPeriodEnd ? (
            <div>
              <AuditAction
                trigger={<Button tone="ghost" className="text-danger">{t("biz.customers.cancelPlan")}</Button>}
                title={t("biz.customers.cancelTitle")}
                description={t("biz.customers.cancelExplain")}
                confirmLabel={t("biz.customers.cancelConfirm")}
                tone="danger"
                reasonLabel={t("biz.reason")}
                isPending={cancel.isPending}
                error={cancel.error}
                onConfirm={() => cancel.mutate()}
              />
            </div>
          ) : null}
        </div>
      ) : (
        <p className="text-sm text-muted">{t("biz.customers.noPlan")}</p>
      )}
      {canWrite && !binding ? (
        <form
          className="mt-4 flex flex-col gap-3 border-t border-line pt-4"
          onSubmit={(e) => {
            e.preventDefault();
            if (!chosen) return;
            bind.mutate({
              planVersionId: chosen.planVersionId,
              complimentaryGrant: chosen.priceMinor === 0 ? grant : null,
            });
          }}
        >
          <Field label={t("biz.customers.bindPlan")}>
            <NativeSelect value={target} onChange={(e) => setTarget(e.target.value)} required>
              <option value="">{t("biz.customers.pickPlan")}</option>
              {published.map((p) => (
                <option key={p.planVersionId} value={p.planVersionId}>
                  {p.name} · v{p.version} · <Cr minor={p.priceMinor} currency={display} />
                </option>
              ))}
            </NativeSelect>
          </Field>
          {chosen?.priceMinor === 0 ? (
            <Field label={t("biz.customers.grant")} hint={t("biz.customers.grantHint")}>
              <TextInput value={grant} onChange={(e) => setGrant(e.target.value)} required />
            </Field>
          ) : null}
          {bind.error instanceof ApiError ? <Banner tone="error">{bind.error.message}</Banner> : null}
          <Button type="submit" isLoading={bind.isPending} disabled={!chosen}>
            {t("biz.customers.bindConfirm")}
          </Button>
        </form>
      ) : null}
    </Card>
  );
}

function CustomerDetail({ customer, onBack }: { customer: BusinessCustomer; onBack: () => void }) {
  const { t } = useTranslation();
  const display = useDisplay();
  const biz = useBusiness().data;
  const canWrite = useCapability(biz, "customersWrite");
  const canSuspend = useCapability(biz, "customersSuspend");
  const canReports = useCapability(biz, "reportsRead");
  const detail = useBusinessCustomer(customer.organizationId);
  const statements = useBusinessStatements({ organizationId: customer.organizationId }, canReports);
  const orders = useBusinessOrders({ organizationId: customer.organizationId }, canReports);
  const update = useUpdateCustomer(customer.organizationId);
  const lifecycle = useCustomerLifecycle(customer.organizationId);
  const suspension = useCustomerSuspension(customer.organizationId);

  const c = detail.data ?? customer;
  const [name, setName] = useState(c.name);
  const [email, setEmail] = useState(c.contactEmail ?? "");

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <div>
          <button type="button" className="text-sm text-primary" onClick={onBack}>
            {t("biz.customers.back")}
          </button>
          <h2 className="mt-1 text-xl font-bold">{c.name}</h2>
        </div>
        {stateBadge(c.state)}
      </div>

      {/* profile */}
      <Card title={t("biz.customers.profile")}>
        <DetailGrid>
          <Field2 label={t("biz.customers.colExternal")}>
            <span className="mono">{c.externalId}</span>
          </Field2>
          <Field2 label={t("biz.customers.colEmail")}>{c.contactEmail ?? "—"}</Field2>
          <Field2 label={t("biz.customers.manualSuspension")}>
            {c.manualSuspendedAt ? c.manualSuspendedAt.slice(0, 10) : "—"}
          </Field2>
          <Field2 label={t("biz.customers.dunningSuspension")}>
            {c.dunningSuspendedAt ? c.dunningSuspendedAt.slice(0, 10) : "—"}
          </Field2>
        </DetailGrid>
        {canWrite ? (
          <form
            className="mt-4 flex flex-col gap-3 border-t border-line pt-4"
            onSubmit={(e) => {
              e.preventDefault();
              update.mutate({ idempotencyKey: idem(), name, contactEmail: email || null, reason: "console edit" });
            }}
          >
            <Field label={t("biz.customers.colName")}>
              <TextInput value={name} onChange={(e) => setName(e.target.value)} required />
            </Field>
            <Field label={t("biz.customers.colEmail")}>
              <TextInput type="email" value={email} onChange={(e) => setEmail(e.target.value)} />
            </Field>
            {update.error instanceof ApiError ? <Banner tone="error">{update.error.message}</Banner> : null}
            <Button type="submit" isLoading={update.isPending}>{t("common.save")}</Button>
          </form>
        ) : null}
        <div className="mt-4 flex flex-wrap gap-2 border-t border-line pt-4">
          {canSuspend ? (
            <AuditAction
              trigger={
                <Button tone="ghost">{c.manualSuspendedAt ? t("biz.customers.unsuspend") : t("biz.customers.suspend")}</Button>
              }
              title={c.manualSuspendedAt ? t("biz.customers.unsuspendTitle") : t("biz.customers.suspendTitle")}
              description={c.manualSuspendedAt ? t("biz.customers.unsuspendExplain") : t("biz.customers.suspendExplain")}
              confirmLabel={t("common.confirm")}
              reasonLabel={t("biz.reason")}
              isPending={suspension.isPending}
              error={suspension.error}
              onConfirm={(reason, key) =>
                suspension.mutate({ idempotencyKey: key, suspended: !c.manualSuspendedAt, reason })
              }
            />
          ) : null}
          {canWrite && c.state === "active" ? (
            <AuditAction
              trigger={<Button tone="danger">{t("biz.customers.archive")}</Button>}
              title={t("biz.customers.archiveTitle")}
              description={t("biz.customers.archiveExplain")}
              confirmLabel={t("biz.customers.archiveConfirm")}
              tone="danger"
              reasonLabel={t("biz.reason")}
              isPending={lifecycle.isPending}
              error={lifecycle.error}
              onConfirm={(reason, key) =>
                lifecycle.mutate({ idempotencyKey: key, state: "archived", reason })
              }
            />
          ) : null}
        </div>
      </Card>

      {canReports ? <CustomerCreditCard key={c.organizationId} customer={c} /> : null}
      <CustomerPlanCard customer={c} />
      {canReports ? <CustomerMonthlyStatements key={c.organizationId} customer={c.organizationId} active={c.state === "active"} /> : null}

      {canReports ? <>
      {/* statements */}
      <Card title={t("biz.customers.statements")}>
        <TableShell
          head={[t("biz.statements.colId"), t("common.status"), { label: t("common.total"), align: "right" }, { label: t("biz.statements.colOwed"), align: "right" }, t("biz.statements.colDue")]}
          loading={statements.isLoading ? 3 : 0}
        >
          {!statements.isLoading &&
            (statements.data?.statements ?? []).map((s) => (
              <tr key={s.statementId}>
                <Td mono truncate>{s.statementId.slice(0, 8)}</Td>
                <td>{stateBadge(s.state)}</td>
                <Td mono align="right"><Cr minor={s.totalMinor} currency={display} /></Td>
                <Td mono align="right"><Cr minor={s.totalMinor - s.paidMinor} currency={display} /></Td>
                <td>{s.dueAt?.slice(0, 10) ?? "—"}</td>
              </tr>
            ))}
          {!statements.isLoading && (statements.data?.statements.length ?? 0) === 0 ? (
            <EmptyRows colSpan={5}>{t("biz.customers.noStatements")}</EmptyRows>
          ) : null}
        </TableShell>
      </Card>

      {/* orders */}
      <Card title={t("biz.customers.orders")}>
        <TableShell
          head={[t("biz.payments.colId"), t("biz.payments.colPurpose"), t("common.status"), { label: t("common.amount"), align: "right" }, t("biz.payments.colWhen")]}
          loading={orders.isLoading ? 3 : 0}
        >
          {!orders.isLoading &&
            (orders.data?.orders ?? []).map((o) => (
              <tr key={o.orderId}>
                <Td mono truncate>{o.orderId.slice(0, 8)}</Td>
                <td>{o.purpose}</td>
                <td>{stateBadge(o.status)}</td>
                <Td mono align="right"><Cr minor={o.amountMinor} currency={display} /></Td>
                <td>{o.createdAt.slice(0, 10)}</td>
              </tr>
            ))}
          {!orders.isLoading && (orders.data?.orders.length ?? 0) === 0 ? (
            <EmptyRows colSpan={5}>{t("biz.customers.noOrders")}</EmptyRows>
          ) : null}
        </TableShell>
      </Card>
      </> : null}
    </div>
  );
}

export function BusinessCustomers() {
  const { t } = useTranslation();
  const [picked, setPicked] = useState<BusinessCustomer | null>(null);
  return (
    <div className="flex flex-col gap-6">
      <h2 className="text-xl font-bold">{t("biz.customers.title")}</h2>
      {picked ? (
        <CustomerDetail customer={picked} onBack={() => setPicked(null)} />
      ) : (
        <><CustomerInvitations /><CustomerList onPick={setPicked} /></>
      )}
    </div>
  );
}
