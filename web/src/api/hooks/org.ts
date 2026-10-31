import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../client";
import type { components } from "../schema";
import type { ApiKey, DisplayCurrencyType, Entitlement, Member, UserOrganization } from "./types";

export function useKeys() {
  return useQuery({
    queryKey: ["keys"],
    queryFn: () => api<ApiKey[]>("/org/keys"),
  });
}

export function useCreateKey() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: Partial<ApiKey> & { name?: string }) =>
      api<components["schemas"]["CreatedApiKey"]>("/org/keys", { method: "POST", body }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["keys"] }),
  });
}

export function usePatchKey() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ keyId, ...body }: { keyId: string } & Record<string, unknown>) =>
      api<ApiKey>(`/org/keys/${keyId}`, { method: "PATCH", body }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["keys"] }),
  });
}

export function useRevokeKey() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (keyId: string) => api<ApiKey>(`/org/keys/${keyId}`, { method: "DELETE" }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["keys"] }),
  });
}

/**
 * The "can this key call now" read (issue #501): admission's own checks, peeked
 * — a revoked row skips the poll entirely, since the key can only ever refuse.
 */
export function useEntitlement(keyId: string, enabled = true) {
  return useQuery({
    queryKey: ["entitlement", keyId],
    queryFn: () => api<Entitlement>(`/org/keys/${keyId}/entitlement`),
    enabled,
    refetchInterval: 30_000,
  });
}

export function useMembers() {
  return useQuery({
    queryKey: ["members"],
    queryFn: () => api<{ members: Member[] }>("/org/members"),
  });
}

export function useAddMember() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (email: string) =>
      api<Member>("/org/members", { method: "POST", body: { email } }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["members"] }),
  });
}

export function useUpdateMember() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ userId, ...body }: { userId: string; role?: string; budgetLimitMinor?: number | null }) =>
      api<Member>(`/org/members/${userId}`, { method: "PATCH", body }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["members"] }),
  });
}

export function useRemoveMember() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (userId: string) =>
      api(`/org/members/${userId}`, { method: "DELETE" }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["members"] }),
  });
}

export function useTransferOwnership() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (userId: string) =>
      api("/org/ownership", { method: "POST", body: { userId } }),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["members"] });
      client.invalidateQueries({ queryKey: ["session"] });
    },
  });
}

export function useCreateInvitation() {
  return useMutation({
    mutationFn: () =>
      api<{ url: string }>("/org/invitations", { method: "POST" }),
  });
}

export function useOrganizations() {
  return useQuery({
    queryKey: ["orgs"],
    queryFn: () => api<UserOrganization[]>("/orgs"),
  });
}

export function useCreateOrganization() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (name: string) =>
      api("/orgs", { method: "POST", body: { name } }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["orgs"] }),
  });
}

export function useSwitchOrganization() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (organizationId: string) =>
      api("/session/organization", { method: "POST", body: { organizationId } }),
    onSettled: () => client.invalidateQueries(),
  });
}

export function useOrg() {
  return useQuery({
    queryKey: ["org"],
    queryFn: () =>
      api<{
        id: string;
        name: string;
        tenantId: string;
        kind: string;
        createdAt: string;
        // The fiat unit this org renders in (issue #257); null = credits only.
        displayCurrency: DisplayCurrencyType | null;
      }>("/org"),
  });
}

/** The organization's display currency — `null` renders credits only. Every
 * money cell that wants the conversion reads the same cached org row. */
export function useDisplay() {
  const org = useOrg();
  return org.data?.displayCurrency ?? null;
}

/** `PATCH /api/v1/org` — the owner-or-admin write that sets (or, with `null`,
 * clears) the display currency. The rate is a decimal string so the wire
 * never carries a float. */
export function useUpdateOrgDisplay() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (displayCurrency: DisplayCurrencyType | null) =>
      api("/org", { method: "PATCH", body: { displayCurrency } }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["org"] }),
  });
}
