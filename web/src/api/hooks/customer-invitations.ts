import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../client";
import type { components } from "../schema";

export type CustomerInvitationSummary = components["schemas"]["CustomerInvitationSummary"];
export type CustomerInvitationRequest = components["schemas"]["CustomerInvitationRequest"];
export type CustomerInvitationResult = components["schemas"]["CustomerInvitationResult"];
export type InvitationDeliveryState = components["schemas"]["InvitationDeliveryState"];

export function useCustomerInvitations(after?: string, enabled = true) {
  return useQuery({
    queryKey: ["biz-invitations", after ?? null], enabled,
    queryFn: () => api<components["schemas"]["CustomerInvitationPage"]>(`/business/customer-invitations${after ? `?after=${encodeURIComponent(after)}` : ""}`),
  });
}
export function useCreateCustomerInvitation() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: CustomerInvitationRequest) => api<CustomerInvitationResult>("/business/customer-invitations", { method: "POST", body }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["biz-invitations"] }),
  });
}
export function useRevokeCustomerInvitation() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ id, ...body }: components["schemas"]["InvitationRevokeRequest"] & { id: string }) =>
      api<components["schemas"]["BusinessMutationResult"]>(`/business/customer-invitations/${id}/revoke`, { method: "POST", body }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["biz-invitations"] }),
  });
}
export function useRegistrationPolicy() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: components["schemas"]["BusinessRegistrationRequest"]) =>
      api<components["schemas"]["BusinessMutationResult"]>("/business/registration", { method: "POST", body }),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["biz-invitations"] });
      client.invalidateQueries({ queryKey: ["auth-methods"] });
    },
  });
}
