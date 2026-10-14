import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "./client";

// The response bodies, typed by hand for the endpoints the scaffold serves —
// `npm run codegen` keeps `api/schema.d.ts` as the authoritative full surface
// (these stay deliberately small; the generated schema is what later pages
// should consume).

export interface SessionInfo {
  user: { id: string; email: string; emailVerified: boolean };
  organization: { id: string; name: string; kind: string; tenantId: string };
  role: "owner" | "admin" | "member";
}

export interface Balance {
  availableMinor: number;
  creditLimitMinor: number;
  creditUsedMinor: number;
}

export interface BillingRecord {
  bookedOn: string;
  requestId: string;
  entryId: string;
  model: string;
  kind: string;
  status: string;
  inputTokens: number;
  outputTokens: number;
  chargedMinor: number;
  discountPercent?: number;
  upstreamAttempts: number;
  keyId?: string;
  failureReason?: string;
}

export interface ApiKey {
  id: string;
  label: string;
  prefix: string;
  createdAt: string;
  revokedAt?: string | null;
  spendLimitMinor?: number | null;
  budgetDuration?: string | null;
}

export interface UsageDay {
  day: string;
  keyId?: string | null;
  channel: string;
  model: string;
  turns: number;
  inputTokens: number;
  outputTokens: number;
  cachedTokens: number;
  reasoningTokens: number;
  chargedMinor: number;
}

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

export function useBalance() {
  return useQuery({ queryKey: ["balance"], queryFn: () => api<Balance>("/balance") });
}

export function useBillingRecords(limit = 50) {
  return useQuery({
    queryKey: ["billing-records", limit],
    queryFn: () =>
      api<{ rows: BillingRecord[]; nextCursor?: number }>(
        `/billing-records?limit=${limit}`,
      ),
  });
}

export function useKeys() {
  return useQuery({
    queryKey: ["keys"],
    queryFn: () => api<{ keys: ApiKey[] } | ApiKey[]>("/org/keys"),
  });
}

export function useUsage(from: string, to: string) {
  return useQuery({
    queryKey: ["usage", from, to],
    queryFn: () => api<{ days: UsageDay[] }>(`/usage?from=${from}&to=${to}`),
  });
}
