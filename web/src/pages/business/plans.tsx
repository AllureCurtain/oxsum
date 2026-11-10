import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Layers } from "lucide-react";
import { ApiError } from "../../api/client";
import { decimalMinor } from "../../lib/money";
import {
  useBusinessPlans,
  useCapability,
  useBusiness,
  useDisplay,
  usePublishPlan,
  useRetirePlan,
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
  TableShell,
  Td,
  TextInput,
} from "../../components/ui";
import { AuditAction } from "./biz";

// ---------------------------------------------------------------------------
// /app/business#business/plans — the sold-plan catalogue staff CRUD (T19):
// list every version, publish a new immutable version, retire one. Binding a
// customer happens on the customer's own detail under customers.
// ---------------------------------------------------------------------------

function PublishForm() {
  const { t } = useTranslation();
  const publish = usePublishPlan();
  const plans = useBusinessPlans();
  const [planId, setPlanId] = useState("");
  const [name, setName] = useState("");
  const [price, setPrice] = useState("");
  const [currency, setCurrency] = useState("USD");
  const [quota, setQuota] = useState("");
  const [period, setPeriod] = useState<"calendarMonth" | "anniversary">("calendarMonth");
  const [overage, setOverage] = useState(false);
  const [attempted, setAttempted] = useState(false);
  const [sort] = useState("0");
  const priceMinor = decimalMinor(price, 6, true);
  const quotaMinor = quota.trim() === "" ? null : decimalMinor(quota, 6, true);
  const invalidPrice = priceMinor === null;
  const invalidQuota = quota.trim() !== "" && quotaMinor === null;

  // Distinct planIds already in the catalogue — a version bump reuses one.
  const planIds = Array.from(new Set((plans.data ?? []).map((p) => p.planId)));

  return (
    <Card title={t("biz.plans.publishTitle")}>
      <form
        className="flex flex-col gap-3"
        onSubmit={(e) => {
          e.preventDefault();
          setAttempted(true);
          if (priceMinor === null || invalidQuota) return;
          publish.mutate(
            {
              planId: planId || null,
              name,
              priceMinor,
              currency,
              quotaMinor,
              periodKind: period,
              overageEnabled: overage,
              sortOrder: Number(sort) || 0,
            },
            {
              onSuccess: () => {
                setName("");
                setPrice("");
                setQuota("");
                setAttempted(false);
              },
            },
          );
        }}
      >
        <Field label={t("biz.plans.colName")}>
          <TextInput value={name} onChange={(e) => setName(e.target.value)} required />
        </Field>
        <div className="grid grid-cols-2 gap-3">
          <Field label={t("biz.plans.colPrice")} hint={t("biz.plans.creditsHint")}>
            <TextInput inputMode="decimal" maxLength={40} value={price} onChange={(e) => setPrice(e.target.value)} required aria-invalid={attempted && invalidPrice} aria-describedby={attempted && invalidPrice ? "plan-money-error" : undefined} />
          </Field>
          <Field label={t("biz.plans.colCurrency")}>
            <TextInput value={currency} onChange={(e) => setCurrency(e.target.value)} required maxLength={3} />
          </Field>
        </div>
        <div className="grid grid-cols-2 gap-3">
          <Field label={t("biz.plans.colQuota")} hint={t("biz.plans.unmeteredHint")}>
            <TextInput inputMode="decimal" maxLength={40} value={quota} onChange={(e) => setQuota(e.target.value)} aria-invalid={attempted && invalidQuota} aria-describedby={attempted && invalidQuota ? "plan-money-error" : undefined} />
          </Field>
          <Field label={t("biz.plans.colPeriod")}>
            <NativeSelect value={period} onChange={(e) => setPeriod(e.target.value as typeof period)}>
              <option value="calendarMonth">{t("plans.period.calendarMonth")}</option>
              <option value="anniversary">{t("plans.period.anniversary")}</option>
            </NativeSelect>
          </Field>
        </div>
        {planIds.length > 0 ? (
          <Field label={t("biz.plans.versionOf")} hint={t("biz.plans.versionOfHint")}>
            <NativeSelect value={planId} onChange={(e) => setPlanId(e.target.value)}>
              <option value="">{t("biz.plans.newPlan")}</option>
              {planIds.map((id) => (
                <option key={id} value={id}>
                  {(plans.data ?? []).find((p) => p.planId === id)?.name ?? id.slice(0, 8)}
                </option>
              ))}
            </NativeSelect>
          </Field>
        ) : null}
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={overage} onChange={(e) => setOverage(e.target.checked)} />
          {t("biz.plans.overage")}
        </label>
        {publish.error instanceof ApiError ? <Banner tone="error">{publish.error.message}</Banner> : null}
        {attempted && (invalidPrice || invalidQuota) ? <p id="plan-money-error" role="alert" className="text-sm text-danger">{t("biz.plans.invalidMoney")}</p> : null}
        <Button type="submit" isLoading={publish.isPending}>{t("biz.plans.publish")}</Button>
      </form>
    </Card>
  );
}

export function BusinessPlans() {
  const { t } = useTranslation();
  const display = useDisplay();
  const plans = useBusinessPlans();
  const retire = useRetirePlan();
  const canWrite = useCapability(useBusiness().data, "commercialWrite");

  return (
    <div className="flex flex-col gap-6">
      <h2 className="text-xl font-bold">{t("biz.plans.title")}</h2>
      {canWrite ? <PublishForm /> : null}
      <Card title={t("biz.plans.catalogue")}>
        {plans.isError ? (
          <Banner tone="error">{t("biz.loadError")}</Banner>
        ) : (
          <TableShell
            head={[
              t("biz.plans.colName"),
              t("biz.plans.colVersion"),
              { label: t("biz.plans.colPrice"), align: "right" },
              { label: t("biz.plans.colQuota"), align: "right" },
              t("biz.plans.colPeriod"),
              t("common.status"),
              "",
            ]}
            loading={plans.isLoading ? 4 : 0}
          >
            {!plans.isLoading &&
              (plans.data ?? []).map((p) => (
                <tr key={p.planVersionId}>
                  <td className="font-medium">{p.name}</td>
                  <Td mono>v{p.version}</Td>
                  <Td mono align="right"><Cr minor={p.priceMinor} currency={display} /></Td>
                  <Td mono align="right">
                    {p.quotaMinor != null ? <Cr minor={p.quotaMinor} currency={display} /> : t("plans.unmetered")}
                  </Td>
                  <td>{t(`plans.period.${p.periodKind}`)}</td>
                  <td>
                    {p.retiredAt ? (
                      <Badge tone="secondary">{t("biz.plans.retired")}</Badge>
                    ) : p.publishedAt ? (
                      <Badge tone="success">{t("biz.plans.published")}</Badge>
                    ) : (
                      <Badge tone="secondary">{t("biz.plans.draft")}</Badge>
                    )}
                  </td>
                  <td className="text-right">
                    {canWrite && p.retiredAt == null ? (
                      <AuditAction
                        trigger={<Button tone="ghost">{t("biz.plans.retire")}</Button>}
                        title={t("biz.plans.retireTitle")}
                        description={t("biz.plans.retireExplain")}
                        confirmLabel={t("biz.plans.retireConfirm")}
                        tone="danger"
                        reasonLabel={t("biz.reason")}
                        isPending={retire.isPending}
                        error={retire.error}
                        onConfirm={() => retire.mutate(p.planVersionId)}
                      />
                    ) : null}
                  </td>
                </tr>
              ))}
            {!plans.isLoading && (plans.data?.length ?? 0) === 0 ? (
              <EmptyRows colSpan={7} icon={<Layers />}>
                {t("biz.plans.empty")}
              </EmptyRows>
            ) : null}
          </TableShell>
        )}
      </Card>
    </div>
  );
}
