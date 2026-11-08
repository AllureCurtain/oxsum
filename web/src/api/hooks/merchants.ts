import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../client";
import type { components } from "../schema";

type Schemas = components["schemas"];
export type MerchantIntent =
  | { operation: "create"; body: Schemas["MerchantCreateRequest"] }
  | { operation: "version"; merchantId: string; body: Schemas["MerchantVersionRequest"] }
  | { operation: "status"; merchantId: string; body: Schemas["MerchantStatusRequest"] };

/** Keep write-only credentials in transient mutation memory only. */
export function useMerchantWrite() {
  const client = useQueryClient();
  return useMutation({
    retry: false,
    gcTime: 0,
    mutationFn: (intent: MerchantIntent) => api<Schemas["MerchantMutationResult"]>(
      intent.operation === "create" ? "/business/merchants"
        : `/business/merchants/${encodeURIComponent(intent.merchantId)}/${intent.operation === "version" ? "versions" : "status"}`,
      { method: "POST", body: intent.body },
    ),
    onSuccess: () => Promise.all([
      client.invalidateQueries({ queryKey: ["biz-merchants"] }),
      client.invalidateQueries({ queryKey: ["biz-merchant-versions"] }),
    ]),
  });
}

/** Read every retained immutable version through the server's revision cursor. */
export function useMerchantVersions(merchantId: string, after?: number) {
  return useQuery({
    queryKey: ["biz-merchant-versions", merchantId, after ?? null],
    queryFn: () => api<Schemas["MerchantVersionPage"]>(
      `/business/merchants/${encodeURIComponent(merchantId)}/versions${after === undefined ? "" : `?after=${after}`}`,
    ),
  });
}

export type CheckoutRoute = Schemas["CheckoutRoute"];
export function useCheckoutRoutes() {
  return useQuery({ queryKey: ["biz-checkout-routes"], queryFn: () => api<CheckoutRoute[]>("/business/checkout-routes") });
}
export function useSelectCheckoutRoute() {
  const client = useQueryClient();
  return useMutation({
    retry: false,
    mutationFn: (body: Schemas["CheckoutRouteRequest"]) => api<Schemas["CheckoutRouteMutation"]>("/business/checkout-routes", { method: "POST", body }),
    onSuccess: () => Promise.all([client.invalidateQueries({ queryKey: ["biz-checkout-routes"] }), client.invalidateQueries({ queryKey: ["checkout-options"] })]),
  });
}
