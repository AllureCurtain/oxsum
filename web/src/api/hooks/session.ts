import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../client";
import type { AuthMethods, SessionInfo } from "./types";

export function useSession() {
  return useQuery({
    queryKey: ["session"],
    queryFn: () => api<SessionInfo>("/session"),
    retry: false,
    staleTime: 60_000,
  });
}

export function useLogin() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: { email: string; password: string }) =>
      api<SessionInfo>("/auth/login", { method: "POST", body }),
    onSuccess: () => client.invalidateQueries(),
  });
}

export function useLogout() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: () => api("/auth/logout", { method: "POST" }),
    onSettled: () => client.clear(),
  });
}

export function useAuthMethods() {
  return useQuery({
    queryKey: ["auth-methods"],
    queryFn: () => api<AuthMethods>("/auth/methods"),
    staleTime: 300_000,
    retry: false,
  });
}

export function useDeleteAccount() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (password: string) =>
      api("/users/me", { method: "DELETE", body: { password } }),
    onSettled: () => client.clear(),
  });
}

export function useHealth() {
  return useQuery({
    queryKey: ["healthz"],
    queryFn: async () => {
      const res = await fetch("/healthz", { credentials: "same-origin" });
      return res.ok ? ("ok" as const) : (`unhealthy: ${res.status}` as const);
    },
    refetchInterval: 30_000,
    retry: false,
  });
}
