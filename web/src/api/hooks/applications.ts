import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../client";
import type { components } from "../schema";

export type Application = components["schemas"]["IntegrationApplication"];
export type ApplicationRequest = components["schemas"]["IntegrationApplicationRequest"];
export function useApplications(after?: string) {
  return useQuery({
    queryKey: ["biz-applications", after ?? null],
    queryFn: () => api<components["schemas"]["IntegrationApplicationPage"]>(`/business/applications${after ? `?after=${encodeURIComponent(after)}` : ""}`),
  });
}
export function useCreateApplication() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: ApplicationRequest) => api<Application>("/business/applications", { method: "POST", body }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["biz-applications"] }),
  });
}
