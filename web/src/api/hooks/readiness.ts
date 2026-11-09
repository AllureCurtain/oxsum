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

/** Scoped immediate evidence, including zero-cost successful calls. */
export function useCustomerReceiptReadiness() {
  return useQuery({
    queryKey: ["customer", "readiness", "receipt"],
    queryFn: () => api<components["schemas"]["CustomerReceiptReadiness"]>("/org/readiness/receipt"),
    retry: false,
  });
}

/** One coherent personal key/model/sale/funding snapshot, with no provider request. */
export function useCustomerServingReadiness() {
  return useQuery({
    queryKey: ["customer", "readiness", "serving"],
    queryFn: () => api<components["schemas"]["CustomerServingReadiness"]>("/org/readiness/serving"),
    retry: false,
  });
}
