import { useTranslation } from "react-i18next";
import { CheckCircle2, Circle } from "lucide-react";
import {
  useBusiness,
  useBusinessCustomers,
  useBusinessPlans,
  useBusinessStaff,
  useMerchants,
} from "../../api/hooks";
import { Banner, Card, Skeleton } from "../../components/ui";

// ---------------------------------------------------------------------------
// /app/business#business/setup — the onboarding checklist. There is no
// dedicated checklist endpoint; each step reads its own live section surface,
// so a step flips the moment its fact lands. Each links to its section hash.
// ---------------------------------------------------------------------------

type Step = {
  key: string;
  labelKey: string;
  section: string;
  /** true when the step's own read confirms it. `undefined` while loading. */
  done: boolean | undefined;
  visible: boolean;
};

function Checklist() {
  const { t } = useTranslation();
  const caps = new Set(useBusiness().data?.capabilities ?? []);
  const canStaff = caps.has("staffManage");
  const canMerchants = caps.has("merchantSecretsWrite");
  const canPlans = caps.has("catalogWrite");
  const canCustomers = caps.has("customersRead");
  const staff = useBusinessStaff(canStaff);
  const merchants = useMerchants(undefined, canMerchants);
  const plans = useBusinessPlans(canPlans);
  const customers = useBusinessCustomers(undefined, canCustomers);

  const steps: Step[] = [
    {
      key: "profile",
      labelKey: "biz.setup.steps.profile",
      section: "staff",
      visible: canStaff,
      done: staff.data === undefined ? undefined : staff.data.length > 0,
    },
    {
      key: "staff",
      labelKey: "biz.setup.steps.staff",
      section: "staff",
      visible: canStaff,
      // More than the founding owner means at least one invite landed.
      done: staff.data === undefined ? undefined : staff.data.length > 1,
    },
    {
      key: "merchant",
      labelKey: "biz.setup.steps.merchant",
      section: "integrations",
      visible: canMerchants,
      done: merchants.data === undefined ? undefined : (merchants.data.merchants.length ?? 0) > 0,
    },
    {
      key: "plan",
      labelKey: "biz.setup.steps.plan",
      section: "plans",
      visible: canPlans,
      // A published version exists — `publishedAt` set, not a draft.
      done:
        plans.data === undefined
          ? undefined
          : plans.data.some((p) => p.publishedAt != null),
    },
    {
      key: "customer",
      labelKey: "biz.setup.steps.customer",
      section: "customers",
      visible: canCustomers,
      done: customers.data === undefined ? undefined : customers.data.customers.length > 0,
    },
  ];

  const visible = steps.filter((s) => s.visible);
  const failed = (canStaff && staff.isError) || (canMerchants && merchants.isError) ||
    (canPlans && plans.isError) || (canCustomers && customers.isError);
  const done = visible.filter((s) => s.done === true).length;
  const loading = visible.some((s) => s.done === undefined);

  return (
    <Card title={t("biz.setup.checklist")}>
      {failed ? <Banner tone="error">{t("biz.loadError")}</Banner> : loading ? (
        <div className="flex flex-col gap-3">
          <Skeleton className="h-5 w-56" />
          <Skeleton className="h-5 w-72" />
          <Skeleton className="h-5 w-64" />
        </div>
      ) : (
        <>
          <p className="mb-4 text-sm text-muted">
            {t("biz.setup.progress", { done, total: visible.length })}
          </p>
          <ul className="flex flex-col gap-1">
            {visible.map((s) => (
              <li key={s.key}>
                <a
                  href={`#business/${s.section}`}
                  className="flex min-h-11 items-center gap-3 rounded-lg px-2 py-2 text-sm hover:bg-canvas"
                >
                  {s.done ? (
                    <CheckCircle2 className="size-4.5 shrink-0 text-success" aria-hidden />
                  ) : (
                    <Circle className="size-4.5 shrink-0 text-muted" aria-hidden />
                  )}
                  <span className={s.done ? "text-muted line-through" : "text-ink"}>
                    {t(s.labelKey)}
                  </span>
                  <span className="ml-auto text-xs text-primary">{t("biz.setup.go")}</span>
                </a>
              </li>
            ))}
          </ul>
        </>
      )}
    </Card>
  );
}

export function BusinessSetup() {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-6">
      <h2 className="text-xl font-bold">{t("biz.setup.title")}</h2>
      <Checklist />
      <Card title={t("biz.setup.howTitle")}>
        <ul className="flex list-disc flex-col gap-1.5 pl-5 text-sm text-muted">
          <li>{t("biz.setup.note1")}</li>
          <li>{t("biz.setup.note2")}</li>
          <li>{t("biz.setup.note3")}</li>
        </ul>
      </Card>
    </div>
  );
}
