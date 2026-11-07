import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../client";
import type { components } from "../schema";
import type {
  AuthMethods,
  SessionInfo,
  TotpCodeRequest,
  TotpConfirmResponse,
  TotpEnrollResponse,
  TotpLoginRequest,
  TotpRequired,
  SessionContextInfo,
} from "./types";

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
    // The contract's login answer is a union: `SessionInfo` on a
    // password-only account, `TotpRequired` when a confirmed second factor
    // still gates the session. The caller must branch on `requiresTotp` —
    // only the `SessionInfo` arm should invalidate and navigate away.
    mutationFn: (body: { email: string; password: string }) =>
      api<SessionInfo | SessionContextInfo | TotpRequired>("/auth/login", { method: "POST", body }),
    onSuccess: async (data) => {
      if ("requiresTotp" in data) return;
      await client.cancelQueries();
      client.clear();
      if ("organization" in data) client.setQueryData(["session"], data);
      else client.setQueryData(["session-context"], data);
    },
  });
}

/** The second leg of a TOTP-gated login: the `challengeToken` the first leg
 * answered plus a live six-digit code — or a single-use `xxxx-xxxx` backup
 * code, consumed on use — for the session it withheld. */
export function useTotpLogin() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: TotpLoginRequest) =>
      api<SessionInfo | SessionContextInfo>("/auth/totp", { method: "POST", body }),
    onSuccess: async (data) => {
      await client.cancelQueries();
      client.clear();
      if ("organization" in data) client.setQueryData(["session"], data);
      else client.setQueryData(["session-context"], data);
    },
  });
}

/** Mint a fresh unconfirmed shared secret for the session user — answered
 * once as base32 plus the `otpauthUrl` the page renders. Gates nothing until
 * `useTotpConfirm` proves a live code against it. */
export function useTotpEnroll() {
  return useMutation({
    mutationFn: () => api<TotpEnrollResponse>("/auth/totp/enroll", { method: "POST" }),
  });
}

/** Prove the authenticator scanned the pending secret: on a live code the
 * enrollment confirms and the answer carries ten single-use backup codes,
 * shown exactly once and stored only as hashes. */
export function useTotpConfirm() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: TotpCodeRequest) =>
      api<TotpConfirmResponse>("/auth/totp/confirm", { method: "POST", body }),
    onSuccess: () => client.invalidateQueries(),
  });
}

/** Remove the confirmed factor — itself second-factored: the request carries
 * a live code, so a hijacked session alone cannot strip the protection. */
export function useTotpDisable() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: TotpCodeRequest) =>
      api("/auth/totp/disable", { method: "POST", body }),
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

/** The seller branding (issue #614, T23): a public read — the login and
 * register pages brand themselves before a session exists, so it never waits
 * on one. A deployment that configures nothing answers the compiled-in
 * default, which the callers render as the stock chrome. */
export function useBranding() {
  return useQuery({
    queryKey: ["branding"],
    queryFn: () =>
      api<components["schemas"]["Branding"]>("/branding"),
    staleTime: 300_000,
    retry: false,
  });
}
