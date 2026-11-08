import { useQuery } from "@tanstack/react-query";
import { api } from "../client";
import type { components } from "../schema";

export type BusinessReadiness = components["schemas"]["BusinessReadiness"];

/** Each read rechecks the personal session's current business authority. */
export function useBusinessReadiness() {
  return useQuery({
    queryKey: ["business", "readiness"],
    queryFn: () => api<BusinessReadiness>("/business/readiness"),
    retry: false,
  });
}
