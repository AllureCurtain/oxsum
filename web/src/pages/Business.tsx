import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import { useTranslation } from "react-i18next";
import type { LucideIcon } from "lucide-react";
import {
  ArrowLeftRight,
  ChartColumn,
  FileText,
  Landmark,
  Layers,
  ListChecks,
  Network,
  Receipt,
  ScrollText,
  ShieldCheck,
  Tags,
  Users,
  Wallet,
  Wrench,
} from "lucide-react";
import { ApiError } from "../api/client";
import {
  useBusiness,
  useSessionContext,
  useSwitchContext,
  type BusinessCapability,
} from "../api/hooks";
import { Badge, Banner, Card, PageState } from "../components/ui";
import { BusinessProfileSection } from "./business/profile";
import { BusinessCollection } from "./business/collection";
import { BusinessSetup } from "./business/setup";
import { BusinessCustomers } from "./business/customers";
import { BusinessChannels } from "./business/catalog";
import { BusinessPrices } from "./business/sales";
import { BusinessPlans } from "./business/plans";
import { BusinessBundles } from "./business/bundles";
import { BusinessPayments } from "./business/payments";
import { BusinessRefunds, BusinessDisputes } from "./business/refunds";
import { BusinessReceivables, BusinessStatements } from "./business/statements";
import { BusinessExceptions, BusinessReports } from "./business/reports";
import { BusinessAudit, BusinessIntegrations, BusinessStaffSection } from "./business/staff";

// ---------------------------------------------------------------------------
// /app/business — the staff operations console (issue #615, T24). The session
// selects `business` context explicitly; the principal's `capabilities` map to
// the visible sections, and every page names the active business in chrome.
// ---------------------------------------------------------------------------

type Section = {
  key: string;
  labelKey: string;
  Icon: LucideIcon;
  /** The capability that gates this section — the nav hides what it can't reach. */
  cap: BusinessCapability;
  /** Read-only is enough for these; writes hide behind their own capability. */
};

const SECTION_GROUPS: { group: string; items: Section[] }[] = [
  {
    group: "operations",
    items: [
      { key: "profile", labelKey: "profile", Icon: Wrench, cap: "overviewRead" },
      { key: "setup", labelKey: "setup", Icon: ListChecks, cap: "overviewRead" },
      { key: "collection", labelKey: "collection", Icon: Landmark, cap: "overviewRead" },
      { key: "customers", labelKey: "customers", Icon: Users, cap: "customersRead" },
      { key: "payments", labelKey: "payments", Icon: Wallet, cap: "reportsRead" },
      { key: "refunds", labelKey: "refunds", Icon: ArrowLeftRight, cap: "refundsWrite" },
      { key: "disputes", labelKey: "disputes", Icon: ShieldCheck, cap: "refundsWrite" },
    ],
  },
  {
    group: "receivables",
    items: [
      { key: "statements", labelKey: "statements", Icon: FileText, cap: "reportsRead" },
      { key: "receivables", labelKey: "receivables", Icon: Receipt, cap: "reportsRead" },
      { key: "reports", labelKey: "reports", Icon: ChartColumn, cap: "reportsRead" },
      { key: "exceptions", labelKey: "exceptions", Icon: Wrench, cap: "reportsRead" },
    ],
  },
  {
    group: "catalog",
    items: [
      { key: "channels", labelKey: "channels", Icon: Network, cap: "catalogWrite" },
      { key: "prices", labelKey: "prices", Icon: Tags, cap: "overviewRead" },
      { key: "plans", labelKey: "plans", Icon: Layers, cap: "catalogWrite" },
      { key: "bundles", labelKey: "bundles", Icon: Layers, cap: "overviewRead" },
    ],
  },
  {
    group: "access",
    items: [
      { key: "staff", labelKey: "staff", Icon: Users, cap: "staffManage" },
      { key: "integrations", labelKey: "integrations", Icon: Landmark, cap: "reportsRead" },
      { key: "audit", labelKey: "audit", Icon: ScrollText, cap: "reportsRead" },
    ],
  },
];

const ALL_SECTIONS = SECTION_GROUPS.flatMap((g) => g.items);
const SECTION_KEYS = ALL_SECTIONS.map((s) => s.key);
const HASH_RE = /^#?business\/([a-z-]+)$/;

function sectionFromHash(hash: string): string | null {
  const s = HASH_RE.exec(hash)?.[1];
  return s && SECTION_KEYS.includes(s) ? s : null;
}

function SectionBody({ name }: { name: string }) {
  switch (name) {
    case "profile":
      return <BusinessProfileSection />;
    case "collection":
      return <BusinessCollection />;
    case "setup":
      return <BusinessSetup />;
    case "customers":
      return <BusinessCustomers />;
    case "payments":
      return <BusinessPayments />;
    case "refunds":
      return <BusinessRefunds />;
    case "disputes":
      return <BusinessDisputes />;
    case "statements":
      return <BusinessStatements />;
    case "receivables":
      return <BusinessReceivables />;
    case "reports":
      return <BusinessReports />;
    case "exceptions":
      return <BusinessExceptions />;
    case "channels":
      return <BusinessChannels />;
    case "prices":
      return <BusinessPrices />;
    case "plans":
      return <BusinessPlans />;
    case "bundles":
      return <BusinessBundles />;
    case "staff":
      return <BusinessStaffSection />;
    case "integrations":
      return <BusinessIntegrations />;
    case "audit":
      return <BusinessAudit />;
    default:
      return null;
  }
}

export function Business() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const context = useSessionContext();
  const switchContext = useSwitchContext();
  const inBusiness = context.data?.context === "business";
  const principal = useBusiness(inBusiness);

  // The session must explicitly select business authority — a customer
  // session with staff membership still lands here, but only after it picks
  // `business`. A non-staff session gets 403 from the backend regardless.
  const [section, setSectionState] = useState<string>(
    () => sectionFromHash(location.hash) ?? "setup",
  );
  const setSection = (s: string) => {
    setSectionState(s);
    navigate({ hash: `business/${s}` }, { replace: true });
  };
  useEffect(() => {
    const selected = sectionFromHash(location.hash);
    if (selected) setSectionState(selected);
  }, [location.hash]);

  // Auto-select business context once the session says it can — staff
  // membership is the only path to a non-null `business` on the context read.
  useEffect(() => {
    if (context.data && context.data.context !== "business" && context.data.business != null) {
      switchContext.mutate("business");
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [context.data?.context, context.data?.business]);

  if (context.isLoading) {
    return <PageState>{t("common.loading")}</PageState>;
  }
  // No staff membership at all — the context read carries `business: null`.
  if (context.data && context.data.business == null) {
    return (
      <div className="mx-auto mt-24 max-w-sm">
        <Card title={t("biz.console")}>
          <Banner tone="error">{t("biz.notStaff")}</Banner>
        </Card>
      </div>
    );
  }
  if (!inBusiness) {
    return (
      <div className="mx-auto mt-24 max-w-sm">
        <Card title={t("biz.console")}>
          <p className="text-sm text-muted">{t("biz.selecting")}</p>
          {switchContext.error instanceof ApiError ? (
            <Banner tone="error" className="mt-3">{switchContext.error.message}</Banner>
          ) : null}
        </Card>
      </div>
    );
  }
  if (principal.isLoading) {
    return <PageState>{t("common.loading")}</PageState>;
  }
  if (principal.isError || !principal.data) {
    return (
      <div className="mx-auto mt-24 max-w-sm">
        <Card title={t("biz.console")}>
          <Banner tone="error">
            {principal.error instanceof ApiError ? principal.error.message : t("biz.loadError")}
          </Banner>
        </Card>
      </div>
    );
  }

  const caps = new Set(principal.data.capabilities);
  const visible = SECTION_GROUPS.map((g) => ({
    group: g.group,
    items: g.items.filter((i) => caps.has(i.cap)),
  })).filter((g) => g.items.length > 0);
  const allowedKeys = new Set(visible.flatMap((g) => g.items.map((i) => i.key)));
  const active = allowedKeys.has(section) ? section : (visible[0]?.items[0]?.key ?? "setup");

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold">{t("biz.title")}</h1>
          <p className="mt-0.5 text-xs text-muted">
            {t("biz.contextLine", {
              role: t(`biz.roles.${principal.data.role}`),
              business: principal.data.businessId.slice(0, 8),
            })}
          </p>
        </div>
        <Badge tone="default">{t(`biz.roles.${principal.data.role}`)}</Badge>
      </div>

      <div className="flex flex-col gap-4 lg:flex-row lg:gap-6">
        <nav
          aria-label={t("biz.sections")}
          className="flex shrink-0 flex-row gap-4 overflow-x-auto lg:w-48 lg:flex-col lg:gap-4"
        >
          {visible.map((group) => (
            <div key={group.group} className="min-w-max lg:min-w-0">
              <p className="mb-1 px-2 text-[11px] font-semibold uppercase tracking-wider text-muted">
                {t(`biz.groups.${group.group}`)}
              </p>
              <div className="flex gap-0.5 lg:flex-col">
                {group.items.map(({ key, labelKey, Icon }) => {
                  const on = active === key;
                  return (
                    <button
                      key={key}
                      type="button"
                      aria-current={on ? "page" : undefined}
                      onClick={() => setSection(key)}
                      className={`flex min-h-11 items-center gap-2 whitespace-nowrap rounded-lg px-2.5 py-2 text-[13px] transition-colors ${
                        on
                          ? "bg-primary-soft font-medium text-primary"
                          : "text-muted hover:bg-canvas hover:text-ink"
                      }`}
                    >
                      <Icon className="h-4 w-4 shrink-0" strokeWidth={1.75} />
                      {t(`biz.sectionNav.${labelKey}`)}
                    </button>
                  );
                })}
              </div>
            </div>
          ))}
        </nav>
        <div className="min-w-0 flex-1">
          <SectionBody name={active} />
        </div>
      </div>
    </div>
  );
}
