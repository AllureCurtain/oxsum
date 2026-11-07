import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../client";
import type { components } from "../schema";

export type BusinessProfile = components["schemas"]["BusinessProfile"];
export type BusinessProfileRequest = components["schemas"]["BusinessProfileRequest"];

export function useBusinessProfile() {
  return useQuery({
    queryKey: ["business-profile"],
    queryFn: () => api<BusinessProfile>("/business/profile"),
    retry: false,
  });
}

export function useSaveBusinessProfile() {
  const client = useQueryClient();
  return useMutation({
    retry: false,
    mutationFn: (body: BusinessProfileRequest) =>
      api<components["schemas"]["BusinessProfileMutation"]>("/business/profile", { method: "POST", body }),
    onSuccess: async () => {
      // A replay may describe an older revision; always read the current row.
      await Promise.all([
        client.invalidateQueries({ queryKey: ["business-profile"] }),
        client.invalidateQueries({ queryKey: ["branding"] }),
      ]);
    },
  });
}
