import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
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
// Rides React Query like the customer hooks: `token`/`path`/`deps` form the
// key, so two components asking for the same resource share one fetch and a
// write's `refresh()` refetches from cache rather than re-mounting state.
// `poll` maps the latest data to a refetch interval in ms (`false` stops) —
// the console polls a background job this way while it runs.
export function useAdmin<T>(
  token: string,
  path: string,
  deps: unknown[] = [],
  enabled = true,
  poll?: (data: T | undefined) => number | false,
) {
  const query = useQuery<T, ApiError>({
    queryKey: ["admin", token, path, ...deps],
    queryFn: () => adminApi<T>(token, path),
    enabled,
    refetchInterval: poll ? (query) => poll(query.state.data) : undefined,
    // A POST the caller drives (a form save, an action) lands outside this
    // hook, so the list it should re-read gets a `refresh()` that bypasses any
    // dedup window — `staleTime: 0` keeps every `refresh` a real fetch.
    staleTime: 0,
  });
  return {
    data: query.data ?? null,
    error: query.error instanceof ApiError ? query.error.message : query.error ? "failed" : null,
    loading: query.isPending,
    // Wrap refetch so an `onClick={x.refresh}` passes a MouseEvent, not
    // React Query's RefetchOptions — the old () => setTick shape is what
    // every call site still expects.
    refresh: () => {
      void query.refetch();
    },
  };
}
