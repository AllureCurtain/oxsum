import { NavLink, Outlet, useNavigate } from "react-router-dom";
import { useLogout, useSession } from "../api/hooks";
import { ApiError } from "../api/client";
import { useEffect } from "react";

const NAV = [
  { to: "/", label: "Overview", end: true },
  { to: "/bills", label: "Bills" },
  { to: "/requests", label: "Requests" },
  { to: "/usage", label: "Usage" },
  { to: "/keys", label: "API keys" },
  { to: "/members", label: "Members" },
  { to: "/webhooks", label: "Webhooks" },
  { to: "/statements", label: "Statements" },
] as const;

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
  const { user, organization, role } = session.data;
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
      </nav>
      <main className="max-w-5xl flex-1 px-6 py-6">
        <div className="mb-4 flex items-center justify-end gap-3 text-xs text-muted">
          <span>
            {organization.name} · {user.email} · {role}
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
