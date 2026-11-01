import { NavLink, Outlet, useLocation, useNavigate } from "react-router-dom";
import type { LucideIcon } from "lucide-react";
import {
  ArrowLeftRight,
  ChartColumn,
  ChevronRight,
  FileText,
  KeyRound,
  LayoutDashboard,
  LogOut,
  Menu,
  MessageSquare,
  Newspaper,
  Receipt,
  ScrollText,
  Settings,
  ShieldCheck,
  Tags,
  Users,
  Wallet,
  Webhook,
  X,
} from "lucide-react";
import {
  useCreateOrganization,
  useLogout,
  useOrganizations,
  useSession,
  useSwitchOrganization,
} from "../api/hooks";
import { ApiError } from "../api/client";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, IconButton, TextInput } from "./ui";
import { LanguageSwitcher } from "./LanguageSwitcher";
import { LiveIsland } from "./LiveIsland";

// Nav labels are i18n keys under `nav.*`, resolved with `t()` at render so the
// rail follows the header's language switch.
const NAV_GROUPS = [
  {
    labelKey: "nav.main",
    items: [
      { to: "/", labelKey: "nav.overview", end: true, Icon: LayoutDashboard },
      { to: "/topup", labelKey: "nav.topup", Icon: Wallet },
      { to: "/bills", labelKey: "nav.bills", Icon: Receipt },
      { to: "/requests", labelKey: "nav.requests", Icon: ArrowLeftRight },
      { to: "/usage", labelKey: "nav.usage", Icon: ChartColumn },
    ],
  },
  {
    labelKey: "nav.configure",
    items: [
      { to: "/keys", labelKey: "nav.apiKeys", Icon: KeyRound },
      { to: "/members", labelKey: "nav.members", Icon: Users },
      { to: "/webhooks", labelKey: "nav.webhooks", Icon: Webhook },
      { to: "/pricing", labelKey: "nav.pricing", Icon: Tags },
    ],
  },
  {
    labelKey: "nav.account",
    items: [
      { to: "/statements", labelKey: "nav.statements", Icon: FileText },
      { to: "/reports", labelKey: "nav.reports", Icon: Newspaper },
      { to: "/log", labelKey: "nav.log", Icon: ScrollText },
      { to: "/chat", labelKey: "nav.chat", Icon: MessageSquare },
      { to: "/settings", labelKey: "nav.settings", Icon: Settings },
    ],
  },
] as const;

const ADMIN_ITEM = { to: "/admin", labelKey: "nav.admin", Icon: ShieldCheck } as const;

type NavItem = { to: string; labelKey: string; end?: boolean; Icon: LucideIcon };

// One flat index the breadcrumb and the drawer both resolve against — keys,
// resolved to text by `t()` where the crumbs render.
const NAV_INDEX: Record<string, { labelKey: string; parent?: string }> = Object.fromEntries(
  NAV_GROUPS.flatMap((g) => g.items.map((i) => [i.to, { labelKey: i.labelKey }])),
);
NAV_INDEX["/device"] = { labelKey: "nav.device" };
NAV_INDEX["/admin"] = { labelKey: "nav.admin" };
NAV_INDEX["/statements/:id"] = { labelKey: "nav.statement", parent: "/statements" };

/** Pathname (+hash) → breadcrumb trail, e.g. /statements/s_123 → [Statements, s_123]. */
function breadcrumb(
  pathname: string,
  hash: string,
  t: (key: string) => string,
): { label: string; to?: string }[] {
  const path = pathname.replace(/^\/app\/?/, "/") || "/";
  const segments = path.split("/").filter(Boolean);

  // The Admin console addresses its sections through the hash
  // (#admin/<section>); the hash segment becomes the leaf crumb.
  const adminSection = /^#?admin\/([a-z-]+)$/.exec(hash)?.[1];
  if (path === "/admin" && adminSection) {
    return [{ label: t("nav.admin"), to: "/admin" }, { label: adminSection }];
  }

  if (segments.length > 1) {
    // Depth ≥2: link the parent list page, show the id segment verbatim.
    const parent = `/${segments[0]}`;
    const crumbs: { label: string; to?: string }[] = [];
    if (NAV_INDEX[parent]) {
      crumbs.push({ label: t(NAV_INDEX[parent].labelKey), to: parent });
    }
    crumbs.push({ label: segments.slice(1).join("/") });
    return crumbs;
  }
  const entry = NAV_INDEX[path];
  if (!entry) return [];
  if (entry.parent && NAV_INDEX[entry.parent]) {
    return [
      { label: t(NAV_INDEX[entry.parent].labelKey), to: entry.parent },
      { label: t(entry.labelKey) },
    ];
  }
  return [{ label: t(entry.labelKey) }];
}

function OrgSwitcher() {
  const { t } = useTranslation();
  const orgs = useOrganizations();
  const switchOrg = useSwitchOrganization();
  const create = useCreateOrganization();
  const session = useSession();
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");

  const current = session.data?.organization.id;
  return (
    <div className="relative">
      <button
        type="button"
        className="min-h-11 rounded-lg border border-line bg-surface px-2.5 py-1.5 text-left text-xs hover:bg-canvas sm:min-h-0"
        onClick={() => setOpen((v) => !v)}
      >
        {session.data?.organization.name ?? "…"} ▾
      </button>
      {open ? (
        <div className="absolute right-0 z-10 mt-1 w-64 rounded-lg border border-line bg-surface p-3 shadow-sm">
          <ul className="flex flex-col gap-1">
            {(orgs.data ?? []).map((entry) => (
              <li key={entry.organization.id}>
                <button
                  type="button"
                  className={`min-h-11 w-full rounded-md px-2.5 py-1.5 text-left text-sm hover:bg-canvas sm:min-h-0 ${
                    entry.organization.id === current ? "font-semibold text-primary" : ""
                  }`}
                  onClick={() => {
                    switchOrg.mutate(entry.organization.id);
                    setOpen(false);
                  }}
                >
                  {entry.organization.name}{" "}
                  <span className="text-xs text-muted">({entry.role})</span>
                </button>
              </li>
            ))}
          </ul>
          <form
            className="mt-2 flex gap-1.5 border-t border-line pt-2"
            onSubmit={(e) => {
              e.preventDefault();
              create.mutate(name, { onSuccess: () => setName("") });
            }}
          >
            <TextInput
              placeholder={t("nav.newTeamOrg")}
              value={name}
              onChange={(e) => setName(e.target.value)}
              required
            />
            <Button type="submit" tone="ghost" disabled={create.isPending}>
              {t("common.create")}
            </Button>
          </form>
        </div>
      ) : null}
    </div>
  );
}

function NavItems({ onNavigate }: { onNavigate?: () => void }) {
  const { t } = useTranslation();
  const navLink = ({ isActive }: { isActive: boolean }) =>
    `flex min-h-11 items-center gap-2.5 rounded-lg px-2.5 py-2 text-[13.5px] transition-colors ${
      isActive
        ? "bg-primary font-medium text-white"
        : "text-rail-ink hover:bg-rail-2 hover:text-rail-ink-active"
    }`;
  return (
    <>
      <div className="flex flex-1 flex-col gap-4">
        {NAV_GROUPS.map((group) => (
          <div key={group.labelKey}>
            <p className="mb-1 px-2.5 text-[11px] font-semibold uppercase tracking-wider text-rail-ink opacity-50">
              {t(group.labelKey)}
            </p>
            <div className="flex flex-col gap-0.5">
              {group.items.map(({ to, labelKey, Icon, ...rest }: NavItem & { end?: boolean }) => (
                <NavLink
                  key={to}
                  to={to}
                  end={rest.end}
                  className={navLink}
                  onClick={onNavigate}
                >
                  <Icon className="h-[18px] w-[18px] shrink-0" strokeWidth={1.75} />
                  {t(labelKey)}
                </NavLink>
              ))}
            </div>
          </div>
        ))}
      </div>
      <div className="mt-4 border-t border-white/6 pt-3">
        <NavLink to={ADMIN_ITEM.to} className={navLink} onClick={onNavigate}>
          <ADMIN_ITEM.Icon className="h-[18px] w-[18px] shrink-0" strokeWidth={1.75} />
          {t(ADMIN_ITEM.labelKey)}
        </NavLink>
      </div>
    </>
  );
}

export function AppShell() {
  const { t } = useTranslation();
  const session = useSession();
  const logout = useLogout();
  const navigate = useNavigate();
  const location = useLocation();
  const [drawerOpen, setDrawerOpen] = useState(false);

  // No session → the login page; `GET /session` answers 401 for an
  // unauthenticated caller, which useSession surfaces as an ApiError.
  useEffect(() => {
    if (session.error instanceof ApiError && session.error.status === 401) {
      navigate("/login", { replace: true });
    }
  }, [session.error, navigate]);

  // Route changes always close the drawer.
  useEffect(() => {
    setDrawerOpen(false);
  }, [location.pathname]);

  // Escape closes the drawer too.
  useEffect(() => {
    if (!drawerOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setDrawerOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [drawerOpen]);

  if (session.isLoading) {
    return <p className="p-8 text-muted">{t("common.loading")}</p>;
  }
  if (!session.data) {
    return null;
  }
  const { user, role } = session.data;
  const crumbs = breadcrumb(location.pathname, location.hash, t);

  return (
    <div className="flex min-h-screen">
      {/* ---- mobile drawer (<640px) ---- */}
      {drawerOpen ? (
        <div className="fixed inset-0 z-40 sm:hidden">
          <div
            className="absolute inset-0 bg-ink/40"
            onClick={() => setDrawerOpen(false)}
            aria-hidden
          />
          <nav
            className="absolute inset-y-0 left-0 flex w-64 flex-col overflow-y-auto bg-rail px-3 py-5"
            aria-label={t("nav.mainNav")}
          >
            <div className="mb-5 flex items-center justify-between px-2.5">
              <p className="text-xl font-bold tracking-tight text-white">
                ox<span className="text-primary">sum</span>
              </p>
              <IconButton
                label={t("nav.closeNav")}
                className="border-transparent bg-transparent text-rail-ink hover:bg-rail-2 hover:text-white"
                onClick={() => setDrawerOpen(false)}
              >
                <X size={20} />
              </IconButton>
            </div>
            <NavItems />
          </nav>
        </div>
      ) : null}

      {/* ---- rail: 224px ≥1024px, icon strip 640–1024px, drawer below ---- */}
      <nav className="sticky top-0 hidden h-screen w-16 shrink-0 flex-col overflow-y-auto bg-rail px-3 py-5 sm:flex lg:w-56">
        <p className="mb-5 hidden px-2.5 text-xl font-bold tracking-tight text-white lg:block">
          ox<span className="text-primary">sum</span>
        </p>
        <p className="mb-5 px-2.5 text-xl font-bold tracking-tight text-white lg:hidden">
          o<span className="text-primary">.</span>
        </p>
        <CollapsedNav />
        <ExpandedNav />
      </nav>

      {/* ---- main column ---- */}
      <div className="flex min-w-0 flex-1 flex-col">
        {/* the only sticky element on the page (mobile-floor M5) */}
        <div className="sticky top-0 z-30 flex h-[var(--topbar-h)] items-center justify-between border-b border-line bg-surface px-4 sm:px-6">
          <div className="flex min-w-0 items-center gap-2">
            <IconButton
              label={t("nav.openNav")}
              className="sm:hidden"
              onClick={() => setDrawerOpen(true)}
            >
              <Menu size={20} />
            </IconButton>
            <nav aria-label={t("nav.breadcrumb")} className="min-w-0">
              <ol className="flex items-center gap-1 text-sm">
                {crumbs.map((crumb, i) => (
                  <li key={i} className="flex min-w-0 items-center gap-1">
                    {i > 0 ? <ChevronRight className="h-3.5 w-3.5 shrink-0 text-muted" /> : null}
                    {crumb.to ? (
                      <NavLink
                        to={crumb.to}
                        className="truncate text-muted hover:text-ink"
                      >
                        {crumb.label}
                      </NavLink>
                    ) : (
                      <span
                        aria-current="page"
                        className={`truncate ${i === crumbs.length - 1 && i > 0 ? "mono text-xs" : "text-muted"}`}
                      >
                        {crumb.label}
                      </span>
                    )}
                  </li>
                ))}
              </ol>
            </nav>
          </div>
          <div className="flex shrink-0 items-center gap-2 sm:gap-3">
            <div className="hidden md:block">
              <LiveIsland />
            </div>
            <LanguageSwitcher />
            <OrgSwitcher />
            <span className="hidden text-xs text-muted xl:inline">
              {user.email} · {role}
            </span>
            <IconButton
              label={t("nav.logout")}
              className="sm:hidden"
              onClick={() =>
                logout.mutate(undefined, {
                  onSettled: () => navigate("/login", { replace: true }),
                })
              }
            >
              <LogOut size={18} />
            </IconButton>
            <button
              type="button"
              className="hidden rounded-lg border border-line bg-surface px-2.5 py-1.5 text-xs hover:bg-canvas sm:block"
              onClick={() =>
                logout.mutate(undefined, {
                  onSettled: () => navigate("/login", { replace: true }),
                })
              }
            >
              {t("nav.logout")}
            </button>
          </div>
        </div>

        {/* content */}
        <main className="flex-1 px-4 py-4 sm:px-6 sm:py-6">
          <Outlet />
        </main>
      </div>
    </div>
  );
}

/** 640–1024px: icon-only strip, labels in the tooltip/title. */
function CollapsedNav() {
  const { t } = useTranslation();
  const navLink = ({ isActive }: { isActive: boolean }) =>
    `flex min-h-11 items-center justify-center rounded-lg p-2 transition-colors lg:hidden ${
      isActive
        ? "bg-primary text-white"
        : "text-rail-ink hover:bg-rail-2 hover:text-rail-ink-active"
    }`;
  return (
    <div className="flex flex-1 flex-col">
      {NAV_GROUPS.flatMap<NavItem>((g) => [...g.items]).map(({ to, labelKey, Icon, end }) => (
        <NavLink
          key={to}
          to={to}
          end={end}
          title={t(labelKey)}
          className={navLink}
        >
          <Icon className="h-[18px] w-[18px]" strokeWidth={1.75} />
          <span className="sr-only">{t(labelKey)}</span>
        </NavLink>
      ))}
      <div className="mt-4 border-t border-white/6 pt-3">
        <NavLink to={ADMIN_ITEM.to} title={t(ADMIN_ITEM.labelKey)} className={navLink}>
          <ADMIN_ITEM.Icon className="h-[18px] w-[18px]" strokeWidth={1.75} />
          <span className="sr-only">{t(ADMIN_ITEM.labelKey)}</span>
        </NavLink>
      </div>
    </div>
  );
}

/** ≥1024px: the full icon + label rail. */
function ExpandedNav() {
  return (
    <div className="hidden flex-1 flex-col lg:flex">
      <NavItems />
    </div>
  );
}
