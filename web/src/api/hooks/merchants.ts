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
