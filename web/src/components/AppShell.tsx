import { NavLink, Outlet, useNavigate } from "react-router-dom";
import {
  useCreateOrganization,
  useLogout,
  useOrganizations,
  useSession,
  useSwitchOrganization,
} from "../api/hooks";
import { ApiError } from "../api/client";
import { useEffect, useState } from "react";
import { Button, TextInput } from "./ui";
import { LiveIsland } from "./LiveIsland";

const NAV_GROUPS = [
  {
    label: "Main",
    items: [
      { to: "/", label: "Overview", end: true },
      { to: "/topup", label: "Top up" },
      { to: "/bills", label: "Bills" },
      { to: "/requests", label: "Requests" },
      { to: "/usage", label: "Usage" },
    ],
  },
  {
    label: "Configure",
    items: [
      { to: "/keys", label: "API keys" },
      { to: "/members", label: "Members" },
      { to: "/webhooks", label: "Webhooks" },
      { to: "/pricing", label: "Pricing" },
    ],
  },
  {
    label: "Account",
    items: [
      { to: "/statements", label: "Statements" },
      { to: "/reports", label: "Reports" },
      { to: "/log", label: "Log" },
      { to: "/chat", label: "Chat" },
      { to: "/settings", label: "Settings" },
    ],
  },
] as const;

function OrgSwitcher() {
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
        className="rounded-lg border border-line bg-surface px-2.5 py-1.5 text-left text-xs hover:bg-canvas"
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
                  className={`w-full rounded-md px-2.5 py-1.5 text-left text-sm hover:bg-canvas ${
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
              placeholder="New team organization"
              value={name}
              onChange={(e) => setName(e.target.value)}
              required
            />
            <Button type="submit" tone="ghost" disabled={create.isPending}>
              Create
            </Button>
          </form>
        </div>
      ) : null}
    </div>
  );
}

export function AppShell() {
  const session = useSession();
  const logout = useLogout();
  const navigate = useNavigate();

  // No session → the login page; `GET /session` answers 401 for an
  // unauthenticated caller, which useSession surfaces as an ApiError.
  useEffect(() => {
    if (session.error instanceof ApiError && session.error.status === 401) {
      navigate("/login", { replace: true });
    }
  }, [session.error, navigate]);

  if (session.isLoading) {
    return <p className="p-8 text-muted">Loading…</p>;
  }
  if (!session.data) {
    return null;
  }
  const { user, role } = session.data;

  const navLink = ({ isActive }: { isActive: boolean }) =>
    `flex items-center gap-2 rounded-lg px-2.5 py-1.5 text-[13.5px] transition-colors ${
      isActive
        ? "bg-primary font-medium text-white"
        : "text-rail-ink hover:bg-rail-2 hover:text-rail-ink-active"
    }`;

  return (
    <div className="flex min-h-screen">
      {/* ---- dark rail ---- */}
      <nav className="flex w-56 shrink-0 flex-col bg-rail px-3 py-5">
        <p className="mb-5 px-2.5 text-xl font-bold tracking-tight text-white">
          ox<span className="text-primary">sum</span>
        </p>
        <div className="flex flex-1 flex-col gap-4">
          {NAV_GROUPS.map((group) => (
            <div key={group.label}>
              <p className="mb-1 px-2.5 text-[11px] font-semibold uppercase tracking-wider text-rail-ink opacity-50">
                {group.label}
              </p>
              <div className="flex flex-col gap-0.5">
                {group.items.map((item) => (
                  <NavLink
                    key={item.to}
                    to={item.to}
                    end={"end" in item && item.end}
                    className={navLink}
                  >
                    {item.label}
                  </NavLink>
                ))}
              </div>
            </div>
          ))}
        </div>
        <div className="mt-4 border-t border-white/6 pt-3">
          <NavLink to="/admin" className={navLink}>
            Admin
          </NavLink>
        </div>
      </nav>

      {/* ---- main column ---- */}
      <div className="flex min-w-0 flex-1 flex-col">
        {/* topbar */}
        <div className="flex items-center justify-between border-b border-line bg-surface px-6 py-3">
          <p className="text-sm text-muted">
            {(() => {
              const path = location.pathname.replace(/^\/app\/?/, "/") || "/";
              for (const g of NAV_GROUPS)
                for (const i of g.items)
                  if (i.to === path) return i.label;
              if (path === "/admin") return "Admin";
              return "";
            })()}
          </p>
          <div className="flex items-center gap-3 text-xs text-muted">
            <div className="relative h-9 w-48 shrink-0">
              <LiveIsland />
            </div>
            <OrgSwitcher />
            <span>
              {user.email} · {role}
            </span>
            <button
              type="button"
              className="rounded-lg border border-line bg-surface px-2.5 py-1.5 hover:bg-canvas"
              onClick={() =>
                logout.mutate(undefined, {
                  onSettled: () => navigate("/login", { replace: true }),
                })
              }
            >
              Log out
            </button>
          </div>
        </div>

        {/* content */}
        <main className="flex-1 px-6 py-6">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
