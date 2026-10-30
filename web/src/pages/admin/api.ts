import { useEffect, useState } from "react";
import { ApiError } from "../../api/client";

// The admin console: the operator surface, token-gated like the server-rendered
// `/admin` — the token lives in this browser's localStorage and rides
// `Authorization: Bearer` on `/api/v1/admin/*`, never the session cookie.
export const TOKEN_KEY = "oxsum.admin-token";

export function useAdminToken(): [string | null, (token: string | null) => void] {
  const [token, setToken] = useState<string | null>(() => localStorage.getItem(TOKEN_KEY));
  return [
    token,
    (next) => {
      if (next === null) {
        localStorage.removeItem(TOKEN_KEY);
      } else {
        localStorage.setItem(TOKEN_KEY, next);
      }
      setToken(next);
    },
  ];
}

export async function adminApi<T>(token: string, path: string, init?: { method?: string; body?: unknown }): Promise<T> {
  const res = await fetch(`/api/v1/admin${path}`, {
    method: init?.method ?? "GET",
    headers: {
      Authorization: `Bearer ${token}`,
      ...(init?.body !== undefined ? { "Content-Type": "application/json" } : {}),
    },
    body: init?.body !== undefined ? JSON.stringify(init.body) : undefined,
    credentials: "same-origin",
  });
  const text = await res.text();
  const body = text ? (JSON.parse(text) as { data?: T; error?: { code: string; message: string } }) : {};
  if (!res.ok) {
    throw new ApiError(body.error?.code ?? "UNKNOWN", body.error?.message ?? `HTTP ${res.status}`, res.status);
  }
  return body.data as T;
}

// A tiny query wrapper — the admin surface answers `data` arrays or objects.
export function useAdmin<T>(token: string, path: string, deps: unknown[] = []) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    setLoading(true);
    adminApi<T>(token, path)
      .then((d) => {
        setData(d);
        setError(null);
      })
      .catch((e) => setError(e instanceof ApiError ? e.message : "failed"))
      .finally(() => setLoading(false));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [token, path, tick, ...deps]);
  return { data, error, loading, refresh: () => setTick((t) => t + 1) };
}
