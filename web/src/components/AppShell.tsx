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

const NAV = [
  { to: "/", label: "Overview", end: true },
  { to: "/bills", label: "Bills" },
  { to: "/requests", label: "Requests" },
  { to: "/usage", label: "Usage" },
  { to: "/keys", label: "API keys" },
  { to: "/members", label: "Members" },
  { to: "/webhooks", label: "Webhooks" },
  { to: "/statements", label: "Statements" },
  { to: "/log", label: "Log" },
  { to: "/chat", label: "Chat" },
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
        className="rounded border border-line bg-surface px-2 py-1 text-left text-xs hover:bg-canvas"
        onClick={() => setOpen((v) => !v)}
      >
        {session.data?.organization.name ?? "…"} ▾
      </button>
      {open ? (
        <div className="absolute right-0 z-10 mt-1 w-64 rounded-md border border-line bg-surface p-3 shadow-sm">
          <ul className="flex flex-col gap-1">
            {(orgs.data ?? []).map((entry) => (
              <li key={entry.organization.id}>
                <button
                  type="button"
                  className={`w-full rounded px-2 py-1 text-left text-sm hover:bg-canvas ${
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
            className="mt-2 flex gap-1 border-t border-line pt-2"
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
  return (
    <div className="flex min-h-screen">
      <nav className="flex w-50 shrink-0 flex-col gap-1 border-r border-line bg-surface px-4 py-6">
        <p className="mb-4 px-2 text-xl font-bold">oxsum</p>
        {NAV.map((item) => (
          <NavLink
            key={item.to}
            to={item.to}
            end={"end" in item && item.end}
            className={({ isActive }) =>
              `rounded px-2 py-2 ${
                isActive
                  ? "bg-primary-soft font-semibold text-primary"
                  : "text-ink hover:bg-canvas"
              }`
            }
          >
            {item.label}
          </NavLink>
        ))}
        <div className="mt-6 border-t border-line pt-3">
          <NavLink
            to="/admin"
            className={({ isActive }) =>
              `rounded px-2 py-2 ${
                isActive
                  ? "bg-primary-soft font-semibold text-primary"
                  : "text-ink hover:bg-canvas"
              }`
            }
          >
            Admin
          </NavLink>
        </div>
      </nav>
      <main className="max-w-5xl flex-1 px-6 py-6">
        <div className="mb-4 flex items-center justify-end gap-3 text-xs text-muted">
          <OrgSwitcher />
          <span>
            {user.email} · {role}
          </span>
          <button
            type="button"
            className="rounded border border-line bg-surface px-2 py-1 hover:bg-canvas"
            onClick={() =>
              logout.mutate(undefined, { onSettled: () => navigate("/login", { replace: true }) })
            }
          >
            Log out
          </button>
        </div>
        <Outlet />
      </main>
    </div>
  );
}
