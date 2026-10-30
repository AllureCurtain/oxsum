import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import type { LucideIcon } from "lucide-react";
import {
  Banknote,
  FileCheck,
  KeyRound,
  Landmark,
  Layers,
  ListChecks,
  Receipt,
  Scale,
  ShieldCheck,
  Tags,
  Ticket,
  Users as UsersIcon,
  Wallet,
  Wrench,
  Zap,
} from "lucide-react";
import { Banner, Button, Card, Field, PageState, TextInput } from "../components/ui";
import { adminApi, useAdminToken } from "./admin/api";
import { Channels } from "./admin/channels";
import { Organizations } from "./admin/organizations";
import { Deposits } from "./admin/deposits";
import { InFlight } from "./admin/in-flight";
import { Anomalies } from "./admin/anomalies";
import { Margin } from "./admin/margin";
import { Reconciliation } from "./admin/reconciliation";
import { Closings } from "./admin/closings";
import { AdminStatements } from "./admin/statements";
import { Redemptions } from "./admin/redemptions";
import { Tiers } from "./admin/tiers";
import { Discounts } from "./admin/discounts";
import { Audit } from "./admin/audit";
import { Credentials } from "./admin/credentials";
import { Users } from "./admin/users";
import { Metering } from "./admin/metering";

/**
 * The 16 operator sections grouped under 5 categories — the nav contract the
 * flat 16-tab row broke. Grouping is by what an operator reaches for: the
 * ledger's own record (Ledger), money movement (Money), the price book
 * (Catalog), who can act (Access), and the ops bench (Ops).
 */
const SECTION_GROUPS: { group: string; items: { key: string; label: string; Icon: LucideIcon }[] }[] = [
  {
    group: "Money",
    items: [
      { key: "deposits", label: "Deposits", Icon: Landmark },
      { key: "margin", label: "Margin", Icon: Scale },
      { key: "statements", label: "Statements", Icon: FileCheck },
      { key: "redemptions", label: "Redemptions", Icon: Ticket },
    ],
  },
  {
    group: "Ledger",
    items: [
      { key: "in-flight", label: "In flight", Icon: Zap },
      { key: "reconciliation", label: "Reconciliation", Icon: ListChecks },
      { key: "closings", label: "Closings", Icon: Wallet },
      { key: "audit", label: "Audit", Icon: ShieldCheck },
    ],
  },
  {
    group: "Catalog",
    items: [
      { key: "channels", label: "Channels", Icon: Layers },
      { key: "tiers", label: "Tiers", Icon: UsersIcon },
      { key: "discounts", label: "Discounts", Icon: Tags },
    ],
  },
  {
    group: "Access",
    items: [
      { key: "organizations", label: "Organizations", Icon: Banknote },
      { key: "users", label: "Users", Icon: UsersIcon },
      { key: "credentials", label: "Credentials", Icon: KeyRound },
    ],
  },
  {
    group: "Ops",
    items: [
      { key: "anomalies", label: "Anomalies", Icon: Receipt },
      { key: "metering", label: "Metering", Icon: Wrench },
    ],
  },
];

const SECTIONS = SECTION_GROUPS.flatMap((g) => g.items.map((i) => i.key));

type SectionKey = (typeof SECTIONS)[number];

// Sections address through the URL hash (#admin/<section>) so the breadcrumb
// can show real depth — `Admin / margin` instead of a bare "Admin".
const HASH_RE = /^#?admin\/([a-z-]+)$/;

function sectionFromHash(hash: string): SectionKey | null {
  const match = HASH_RE.exec(hash);
  const s = match?.[1];
  return SECTIONS.includes(s as SectionKey) ? (s as SectionKey) : null;
}

export function Admin() {
  const [token, setToken] = useAdminToken();
  const [section, setSectionState] = useState<SectionKey>(
    () => sectionFromHash(location.hash) ?? "deposits",
  );
  const navigate = useNavigate();

  const setSection = (s: SectionKey) => {
    setSectionState(s);
    navigate({ hash: `admin/${s}` }, { replace: true });
  };

  // The hash can change under us (the breadcrumb links, the back button);
  // keep the section in step.
  useEffect(() => {
    const onHash = () => {
      const s = sectionFromHash(location.hash);
      if (s) setSectionState(s);
    };
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);
  const [draft, setDraft] = useState("");
  const [verified, setVerified] = useState<boolean | null>(null);

  useEffect(() => {
    if (!token) {
      setVerified(null);
      return;
    }
    adminApi(token, "/channels")
      .then(() => setVerified(true))
      .catch(() => setVerified(false));
  }, [token]);

  if (!token) {
    return (
      <div className="mx-auto mt-24 max-w-sm">
        <Card title="Admin console">
          <form
            className="flex flex-col gap-3"
            onSubmit={(e) => {
              e.preventDefault();
              setToken(draft);
            }}
          >
            <Field label="Operator token" hint="OXSUM_ADMIN_TOKEN — checked against the admin surface">
              <TextInput type="password" value={draft} onChange={(e) => setDraft(e.target.value)} required />
            </Field>
            <Button type="submit">Enter</Button>
          </form>
        </Card>
      </div>
    );
  }

  if (verified === false) {
    return (
      <div className="mx-auto mt-24 max-w-sm">
        <Card title="Admin console">
          <Banner tone="error">The token was refused.</Banner>
          <Button tone="ghost" className="mt-3" onClick={() => setToken(null)}>
            Re-enter
          </Button>
        </Card>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Admin</h1>
        <div className="flex items-center gap-3">
          <a href="/metrics" className="text-xs text-muted hover:text-ink" target="_blank" rel="noreferrer">
            /metrics
          </a>
          <Button tone="ghost" onClick={() => setToken(null)}>
            Leave console
          </Button>
        </div>
      </div>
      {verified !== true || !token ? (
        <PageState>Checking token…</PageState>
      ) : (
        <div className="flex flex-col gap-4 lg:flex-row lg:gap-6">
          {/* grouped section nav — stacks above the content below lg */}
          <nav
            aria-label="Admin sections"
            className="flex shrink-0 flex-row gap-4 overflow-x-auto lg:w-44 lg:flex-col lg:gap-4"
          >
            {SECTION_GROUPS.map((group) => (
              <div key={group.group} className="min-w-max lg:min-w-0">
                <p className="mb-1 px-2 text-[11px] font-semibold uppercase tracking-wider text-muted">
                  {group.group}
                </p>
                <div className="flex gap-0.5 lg:flex-col">
                  {group.items.map(({ key, label, Icon }) => {
                    const active = section === key;
                    return (
                      <button
                        key={key}
                        type="button"
                        aria-current={active ? "page" : undefined}
                        onClick={() => setSection(key)}
                        className={`flex min-h-11 items-center gap-2 whitespace-nowrap rounded-lg px-2.5 py-2 text-[13px] transition-colors ${
                          active
                            ? "bg-primary-soft font-medium text-primary"
                            : "text-muted hover:bg-canvas hover:text-ink"
                        }`}
                      >
                        <Icon className="h-4 w-4 shrink-0" strokeWidth={1.75} />
                        {label}
                      </button>
                    );
                  })}
                </div>
              </div>
            ))}
          </nav>
          <div className="min-w-0 flex-1">
            <Section token={token} name={section} />
          </div>
        </div>
      )}
    </div>
  );
}

function Section({ token, name }: { token: string; name: SectionKey }) {
  switch (name) {
    case "channels":
      return <Channels token={token} />;
    case "organizations":
      return <Organizations token={token} />;
    case "deposits":
      return <Deposits token={token} />;
    case "in-flight":
      return <InFlight token={token} />;
    case "anomalies":
      return <Anomalies token={token} />;
    case "margin":
      return <Margin token={token} />;
    case "reconciliation":
      return <Reconciliation token={token} />;
    case "closings":
      return <Closings token={token} />;
    case "statements":
      return <AdminStatements token={token} />;
    case "redemptions":
      return <Redemptions token={token} />;
    case "tiers":
      return <Tiers token={token} />;
    case "discounts":
      return <Discounts token={token} />;
    case "audit":
      return <Audit token={token} />;
    case "credentials":
      return <Credentials token={token} />;
    case "users":
      return <Users token={token} />;
    case "metering":
      return <Metering />;
  }
}
